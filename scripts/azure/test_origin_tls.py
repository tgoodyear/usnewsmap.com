import base64
import gzip
import json
import os
from pathlib import Path
import shutil
import unittest
from unittest.mock import patch
import uuid

if __package__:
    from . import origin_tls as tls
    from . import origin_tls_bootstrap as bootstrap
else:
    import origin_tls as tls
    import origin_tls_bootstrap as bootstrap


class OriginTlsTests(unittest.TestCase):
    def setUp(self):
        self.root = Path.cwd() / (".origin-tls-test-" + uuid.uuid4().hex)
        self.root.mkdir(mode=0o700)
        self.addCleanup(shutil.rmtree, self.root)

    def test_state_roundtrip_preserves_account_and_private_key(self):
        account = self.root / "accounts/acme.example/directory/account"
        account.mkdir(parents=True)
        (account / "private_key.json").write_text('{"private":"account-key"}')
        pem = self.root / "live/usnm-origin"
        pem.mkdir(parents=True)
        for name in ("cert.pem", "chain.pem", "fullchain.pem", "privkey.pem"):
            (pem / name).write_text(name + "-contents")
        restored = self.root / "restored"
        actual = tls.state_restore(tls.state_encode(self.root), restored)
        self.assertEqual((actual / "privkey.pem").read_text(), "privkey.pem-contents")
        self.assertEqual((restored / "accounts/acme.example/directory/account/private_key.json")
                         .stat().st_mode & 0o777, 0o600)

    def test_state_rejects_traversal(self):
        files = {"pem/" + name: "" for name in
                 ("cert.pem", "chain.pem", "fullchain.pem", "privkey.pem")}
        files["accounts/../../escape"] = "private"
        value = base64.b64encode(gzip.compress(json.dumps(files).encode())).decode()
        with self.assertRaises(ValueError):
            tls.state_restore(value, self.root)
        self.assertFalse((self.root.parent / "escape").exists())

    def test_cleanup_deletes_only_matching_challenge_not_other_txt(self):
        with patch.dict(os.environ, CERTBOT_DOMAIN="usnewsmap.com",
                        CERTBOT_VALIDATION="challenge"), patch.object(tls, "cloudflare") as cf:
            cf.return_value = [
                {"id": "ours", "content": "challenge"},
                {"id": "other", "content": "different-challenge"},
            ]
            tls.challenge(cleanup=True)
            self.assertEqual(cf.call_count, 2)
            self.assertEqual(cf.call_args.args,
                             ("DELETE", f"zones/{tls.ZONE}/dns_records/ours"))

    def test_hook_rejects_unexpected_domain_before_reading_token(self):
        with patch.dict(os.environ, CERTBOT_DOMAIN="attacker.example",
                        CERTBOT_VALIDATION="challenge"), patch.object(tls, "cloudflare") as cf:
            with self.assertRaises(ValueError):
                tls.challenge()
            cf.assert_not_called()

    def test_binding_preserves_unrelated_hostname_and_patches_no_app_settings(self):
        domains = [{"name": host, "certificateId": "old"} for host in tls.HOSTS]
        domains.append({"name": "other.usnewsmap.com", "certificateId": "other"})
        app = {"properties": {"configuration": {"ingress": {"customDomains": domains},
                                               "secrets": [{"value": "do-not-replay"}]}}}
        with patch.object(tls, "arm", side_effect=[app, {}, app]) as arm:
            tls.bind()
            body = arm.call_args_list[1].args[2]
            config = body["properties"]["configuration"]
            self.assertEqual(set(config), {"ingress"})
            updated = config["ingress"]["customDomains"]
            self.assertEqual(updated[-1]["certificateId"], "other")
            self.assertTrue(all(item["certificateId"] == tls.CERTIFICATE for item in updated[:3]))

    def test_expiring_dns_token_fails_with_rotation_instruction(self):
        with patch.object(tls, "azure_token", return_value="not-real"), \
                patch.object(tls, "request", return_value={
                    "value": "not-real", "attributes": {"exp": 1},
                }):
            with self.assertRaisesRegex(RuntimeError, "origin_tls_bootstrap"):
                tls.secret(tls.TOKEN_SECRET)

    def test_failed_state_write_never_uploads_or_leaves_private_workdir(self):
        previous = Path.cwd()
        os.chdir(self.root)
        try:
            with patch.object(tls, "secret", side_effect=[
                    "dns-token", None, RuntimeError("denied"),
            ]), \
                    patch.object(tls.subprocess, "run"), \
                    patch.object(tls, "certificate_current", return_value=True), \
                    patch.object(tls, "state_encode", return_value="private-state"), \
                    patch.object(tls, "upload") as upload:
                with self.assertRaisesRegex(RuntimeError, "denied"):
                    tls.renew("admin@example.com", no_bind=True)
                upload.assert_not_called()
                self.assertFalse((self.root / ".origin-tls-work").exists())
        finally:
            os.chdir(previous)

    def test_current_certificate_still_checks_dns_token_expiry(self):
        previous = Path.cwd()
        os.chdir(self.root)
        try:
            with patch.object(tls, "secret", side_effect=RuntimeError(
                    "DNS token expires within 14 days; run origin_tls_bootstrap.py",
            )) as secret, \
                    patch.object(tls, "state_restore", return_value=self.root), \
                    patch.object(tls, "certificate_current", return_value=True), \
                    patch.object(tls, "upload") as upload:
                with self.assertRaisesRegex(RuntimeError, "origin_tls_bootstrap"):
                    tls.renew("admin@example.com", no_bind=True)
                secret.assert_called_once_with(tls.TOKEN_SECRET)
                upload.assert_not_called()
                self.assertFalse((self.root / ".origin-tls-work").exists())
        finally:
            os.chdir(previous)

    def test_current_certificate_checks_token_without_reissuing(self):
        previous = Path.cwd()
        os.chdir(self.root)
        try:
            with patch.object(tls, "secret", side_effect=["dns-token", "saved-state"]) as secret, \
                    patch.object(tls, "state_restore", return_value=self.root), \
                    patch.object(tls, "certificate_current", return_value=True), \
                    patch.object(tls.subprocess, "run") as certbot, \
                    patch.object(tls, "upload") as upload:
                tls.renew("admin@example.com", no_bind=True)
                self.assertEqual(secret.call_args_list[0].args, (tls.TOKEN_SECRET,))
                certbot.assert_not_called()
                upload.assert_called_once_with(self.root)
        finally:
            os.chdir(previous)

    def test_bootstrap_revokes_new_token_if_vault_write_fails(self):
        created = {"success": True, "result": {"id": "new-id", "value": "new-secret"}}
        with patch.object(tls, "azure_token", return_value="azure-token"), \
                patch.object(tls, "request", side_effect=[
                    RuntimeError("GET vault: HTTP 404"), created, {"success": True},
                ]) as request, \
                patch.object(tls, "secret", side_effect=[
                    "operator-token", RuntimeError("PUT vault: HTTP 403"), "operator-token",
                ]):
            with self.assertRaisesRegex(RuntimeError, "HTTP 403"):
                bootstrap.bootstrap_credentials()
            deletion = request.call_args
            self.assertEqual(deletion.args[0], "DELETE")
            self.assertTrue(deletion.args[1].endswith("/tokens/new-id"))

    def test_vault_bootstrap_limits_policy_exception_and_operator_permissions(self):
        created_templates = []

        def az(*args):
            if args[:4] == ("ad", "signed-in-user", "show", "--query"):
                return json.dumps("operator-object-id").encode()
            if args[:3] == ("deployment", "group", "create"):
                path = Path(args[args.index("--template-file") + 1])
                created_templates.append(json.loads(path.read_text()))
            if args[:3] == ("role", "assignment", "list"):
                return b"[]"
            return b"{}"

        with patch.object(bootstrap, "az", side_effect=az) as azure, \
                patch.object(tls, "azure_token", return_value="management-token"), \
                patch.object(tls, "request", side_effect=[
                    RuntimeError("GET management: HTTP 404"), {}, {}, {},
                ]) as request:
            bootstrap.provision_vault()
        vault = created_templates[0]["resources"][0]
        self.assertEqual(vault["properties"]["publicNetworkAccess"], "Disabled")
        self.assertTrue(vault["properties"]["enableRbacAuthorization"])
        diagnostic, exemption, enable_network = request.call_args_list[1:]
        self.assertIn(bootstrap.VAULT_SCOPE + "/providers/Microsoft.Insights/diagnosticSettings/",
                      diagnostic.args[1])
        self.assertEqual(diagnostic.args[3]["properties"], {
            "workspaceId": bootstrap.WORKSPACE,
            "logs": [{"category": "AuditEvent", "enabled": True}],
        })
        self.assertIn(bootstrap.VAULT_SCOPE + "/providers/Microsoft.Authorization/policyExemptions/",
                      exemption.args[1])
        self.assertEqual(exemption.args[3]["properties"]["exemptionCategory"], "Mitigated")
        self.assertEqual(enable_network.args[0], "PATCH")
        self.assertIn(bootstrap.VAULT_SCOPE + "?", enable_network.args[1])
        role_call = azure.call_args_list[-1].args
        self.assertEqual(role_call[role_call.index("--assignee-object-id") + 1],
                         "operator-object-id")
        self.assertEqual(role_call[role_call.index("--scope") + 1], bootstrap.VAULT_SCOPE)
        self.assertFalse(list(Path.cwd().glob(".origin-tls-vault-*.json")))

    def test_bootstrap_cli_pins_subscription_for_resource_writes(self):
        commands = (
            ("deployment", "group", "create", "--resource-group", "rg-usnm-prod"),
            ("role", "assignment", "create", "--scope", bootstrap.VAULT_SCOPE),
            ("role", "definition", "create", "--role-definition", "public-role.json"),
        )
        with patch.object(bootstrap.subprocess, "check_output", return_value=b"{}") as run:
            for args in commands:
                bootstrap.az(*args)
                argv = run.call_args.args[0]
                self.assertEqual(argv[argv.index("--subscription") + 1], tls.SUBSCRIPTION)

    def test_existing_vault_receives_audit_diagnostics_without_recreation(self):
        officer = {"roleDefinitionId": "/providers/Microsoft.Authorization/roleDefinitions/"
                   "b86a8fe4-44ce-4948-aee5-eccb2c155cd7"}
        with patch.object(bootstrap, "az", side_effect=[
                b'"operator"', json.dumps([officer]).encode(),
        ]) as azure, \
                patch.object(tls, "azure_token", return_value="management-token"), \
                patch.object(tls, "request", return_value={}) as request:
            bootstrap.provision_vault()
        self.assertFalse(any(call.args[:3] == ("deployment", "group", "create")
                             for call in azure.call_args_list))
        diagnostic = request.call_args_list[1]
        self.assertEqual(diagnostic.args[0], "PUT")
        self.assertIn("/diagnosticSettings/usnm-origin-tls?", diagnostic.args[1])
        self.assertEqual(diagnostic.args[3]["properties"]["workspaceId"], bootstrap.WORKSPACE)


if __name__ == "__main__":
    unittest.main()
