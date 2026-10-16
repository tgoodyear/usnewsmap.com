"""Invoke the installed pinned BOINC module, not a rewritten connection fragment."""
import argparse
import json
import sys

sys.path.insert(0, "/opt/boinc/py")
from Boinc import db_base
import MySQLdb

parser = argparse.ArgumentParser()
parser.add_argument("--host", required=True)
args = parser.parse_args()
try:
    db_base.do_connect("fixture", "fixture", "fixture-only", 3306, args.host)
except MySQLdb.Error as error:
    print(json.dumps({"mysql_errno": error.args[0]}), file=sys.stderr)
    raise SystemExit(10) from None
cursor = db_base.get_dbconnection().cursor()
cursor.execute("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
row = cursor.fetchone()
assert row and row["Value"], "empty session cipher"
print(row["Value"])
cursor.close()
db_base.close()
