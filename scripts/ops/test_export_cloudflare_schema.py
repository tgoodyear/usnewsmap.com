"""Offline route and request-contract tests for the Cloudflare exporter."""

import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
module_spec = importlib.util.spec_from_file_location(
    "exporter", HERE / "export-cloudflare-schema.py"
)
exporter = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(exporter)


class Export(unittest.TestCase):
    def setUp(self):
        self.router = (ROOT / exporter.ROUTER_PATH).read_text()
        self.spec = (ROOT / exporter.SPEC_PATH).read_text()
        self.doc = {
            "openapi": "3.1.0",
            "info": {"title": "Synthetic API", "version": "fixture"},
            "servers": [
                {"url": "https://api.usnewsmap.com"},
                {"url": "https://usnewsmap.com"},
            ],
            "paths": {},
        }
        entries = re.findall(
            r'^\s*\("(/[^"]+)", (get|post)\([\w:]+\)\),$', self.router, re.M
        )
        self.assertTrue(entries)
        self.expected = {
            (prefix + path, method)
            for path, method in entries
            for prefix in ("/v1", "/api/v1")
        }
        for index, (path, method) in enumerate(entries):
            if method == "get":
                self.doc["paths"]["/v1" + path] = {"get": {"operationId": f"get{index}"}}

    def build(self, doc=None, router=None):
        return exporter.build(
            self.doc if doc is None else doc,
            self.router if router is None else router,
            self.spec, "a" * 40, "fixture.json",
        )

    def test_inventory_and_aliases_follow_the_router(self):
        result = self.build()
        actual = {
            (path, method)
            for path, item in result["paths"].items()
            for method in item
        }
        self.assertEqual(actual, self.expected)
        ids = [
            operation["operationId"]
            for item in result["paths"].values()
            for operation in item.values()
        ]
        self.assertEqual(len(ids), len(set(ids)))
        for path, item in result["paths"].items():
            if path.startswith("/api/"):
                expected = copy.deepcopy(result["paths"][path.removeprefix("/api")])
                for operation in expected.values():
                    operation["operationId"] += "Alias1"
                self.assertEqual(item, expected)
        for path in ("/v1/beacon", "/v1/search-report"):
            self.assertNotIn("requestBody", result["paths"][path]["post"])

    def test_route_and_server_drift_is_rejected(self):
        missing = copy.deepcopy(self.doc)
        del missing["paths"]["/v1/openapi.json"]
        invented = copy.deepcopy(self.doc)
        invented["paths"]["/v1/not-registered"] = invented["paths"]["/v1/meta"]
        wrong_method = copy.deepcopy(self.doc)
        wrong_method["paths"]["/v1/meta"]["post"] = wrong_method["paths"]["/v1/meta"].pop("get")
        for doc in (missing, invented, wrong_method):
            with self.subTest(paths=doc["paths"]):
                with self.assertRaisesRegex(ValueError, "document/router mismatch"):
                    self.build(doc=doc)
        wrong_host = copy.deepcopy(self.doc)
        wrong_host["servers"][0]["url"] = "https://example.com"
        with self.assertRaisesRegex(ValueError, "server mismatch"):
            self.build(doc=wrong_host)
        legacy = 'let v1 = Router::new().route("/meta", get(routes::meta));'
        with self.assertRaisesRegex(ValueError, "unsupported router"):
            self.build(router=legacy)

    def test_missing_internal_route_is_not_invented(self):
        changed = self.router.replace(
            '        ("/search-report", post(routes::search_report)),\n', ""
        )
        self.assertNotEqual(changed, self.router)
        with self.assertRaisesRegex(ValueError, "internal POST routes changed"):
            self.build(router=changed)

    def test_permissive_wire_formats_and_required_parameters(self):
        cases = [
            ("state", {"type": "array", "items": {"pattern": "^[A-Za-z]{2}$"}},
             ["", "ny, CA", "NY%2CCA"], ["N1"]),
            ("limit", {"type": "integer", "minimum": 1, "maximum": 50},
             ["", "+05", "%2B5", "50"], ["0", "51"]),
            ("from", {"type": "string", "format": "date"},
             ["", "1850-1-5", "+1850-01-01"], ["1850/01/01"]),
            ("bucket", {"type": "string", "enum": ["auto", "year"]},
             ["", "auto", "year"], ["decade"]),
        ]
        for name, schema, accepted, rejected in cases:
            with self.subTest(name=name):
                param = exporter.convert_param({"name": name, "in": "query", "schema": schema})
                self.assertEqual(param["schema"]["type"], "string")
                self.assertTrue(param["allowEmptyValue"])
                for value in accepted:
                    self.assertRegex(value, param["schema"]["pattern"])
                for value in rejected:
                    self.assertNotRegex(value, param["schema"]["pattern"])
        required = exporter.convert_param({
            "name": "q", "in": "query", "required": True,
            "schema": {"type": "string", "maxLength": 256},
        })
        self.assertTrue(required["required"])
        self.assertEqual(required["schema"], {"type": "string"})
        path = exporter.convert_param({
            "name": "lccn", "in": "path", "required": True,
            "schema": {"type": "string", "pattern": "^[a-z0-9]{1,16}$"},
        })
        self.assertTrue(path["required"])
        self.assertNotIn("allowEmptyValue", path)
        self.assertNotRegex("NOT-A-TITLE", path["schema"]["pattern"])

    def test_failed_export_preserves_the_previous_upload(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.json"
            source.write_text(json.dumps({**self.doc, "paths": {}}))
            output = Path(directory) / "upload.yaml"
            output.write_text("previous upload\n")
            with patch.object(exporter, "read_source", return_value=(
                self.router, self.spec, "a" * 40
            )), contextlib.redirect_stderr(io.StringIO()) as errors:
                status = exporter.main(["--source", str(source), "--output", str(output)])
            self.assertEqual(status, 1)
            self.assertIn("document/router mismatch", errors.getvalue())
            self.assertEqual(output.read_text(), "previous upload\n")


if __name__ == "__main__":
    unittest.main()
