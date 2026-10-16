#include <cstdio>
#include <cstdlib>
#include "db_base.h"

int main(int argc, char** argv) {
    if (argc != 2) return 2;
    DB_CONN db;
    char name[] = "fixture", user[] = "fixture", password[] = "fixture-only";
    if (db.open(name, argv[1], user, password)) return 10;
    if (db.do_query("SHOW SESSION STATUS LIKE 'Ssl_cipher'")) return 11;
    MYSQL_RES* result = mysql_store_result(db.mysql);
    MYSQL_ROW row = result ? mysql_fetch_row(result) : nullptr;
    if (!row || !row[1] || !*row[1]) return 12;
    std::printf("%s\n", row[1]);
    mysql_free_result(result);
    db.close();
    return 0;
}
