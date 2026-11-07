"""Invoke the installed pinned BOINC module, not a rewritten connection fragment."""
import argparse
import json
import sys

parser = argparse.ArgumentParser()
parser.add_argument("--host", required=True)
parser.add_argument("--project-dir", default="/opt/boinc")
parser.add_argument("--database", default="fixture")
parser.add_argument("--user", default="fixture")
args = parser.parse_args()
sys.path.insert(0, args.project_dir + "/py")
from Boinc import db_base
import MySQLdb

try:
    db_base.do_connect(args.database, args.user, "fixture-only", 3306, args.host)
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
