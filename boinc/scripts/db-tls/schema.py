"""Exercise stock make_project's database creation routine over actual verified TLS."""
import argparse
import json
import sys
from types import SimpleNamespace

sys.path.insert(0, "/opt/boinc/py")
from Boinc import database

parser = argparse.ArgumentParser()
parser.add_argument("--create", action="store_true")
args = parser.parse_args()
config = SimpleNamespace(db_name="boinc_fixture", db_host="tls-db.test",
                         db_user="fixture", db_passwd="fixture-only")
if args.create:
    database.create_database("/opt/boinc", config=config)
else:
    database.connect(config)
connection = database.get_dbconnection()
cursor = connection.cursor()
cursor.execute("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
cipher = cursor.fetchone()["Value"]
assert cipher, "schema lifecycle session has no TLS cipher"
cursor.execute("SHOW TABLES")
tables = {next(iter(row.values())) for row in cursor.fetchall()}
assert {"workunit", "result", "host", "user", "app", "consent_type"} <= tables
cursor.execute("SELECT COUNT(*) AS n FROM consent_type")
assert cursor.fetchone()["n"] == 2, "stock initial content was not preserved"
cursor.close()
database.close()
print(json.dumps({"case": "stock-schema-create" if args.create else "stock-schema-after-restart",
                  "consumer": "python-create_database", "cipher": cipher,
                  "tables": len(tables), "stock_content_rows": 2}))
