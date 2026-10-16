"""Invoke the installed pinned BOINC module, not a rewritten connection fragment."""
import sys

sys.path.insert(0, "/opt/boinc/py")
from Boinc import db_base

db_base.do_connect("fixture", "fixture", "fixture-only", 3306, sys.argv[1])
cursor = db_base.get_dbconnection().cursor()
cursor.execute("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
row = cursor.fetchone()
assert row and row["Value"], "empty session cipher"
print(row["Value"])
cursor.close()
db_base.close()
