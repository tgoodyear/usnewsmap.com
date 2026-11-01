/* Link-time stand-ins for libcurl and libarchive in the static Tesseract (#1418).
 *
 * Debian's libtesseract.a refers to both. Neither is used to read a page: libcurl only
 * fetches an http(s) input, which a task never passes, and libarchive only opens a model
 * packed as an archive, which the locked plain .traineddata files aren't. With these stubs a
 * URL input fails, and TessdataManager::Init falls through to its ordinary file read, the
 * path the container's plain models also end up on. No network library is in the binary.
 */
#include <stddef.h>

void *curl_easy_init(void) { return NULL; }
void curl_easy_cleanup(void *curl) { (void)curl; }
int curl_easy_perform(void *curl) { (void)curl; return 2; }
int curl_easy_setopt(void *curl, int option, ...) { (void)curl; (void)option; return 2; }
const char *curl_easy_strerror(int code) { (void)code; return "this build has no network"; }

/* TessdataManager::LoadArchiveFile stops at the first of these. The others are only reached
 * after a non-null archive, so they're never called. */
void *archive_read_new(void) { return NULL; }
int archive_read_free(void *a) { (void)a; return -30; }
int archive_read_support_filter_all(void *a) { (void)a; return -30; }
int archive_read_support_format_all(void *a) { (void)a; return -30; }
int archive_read_open_filename(void *a, const char *f, size_t n) { (void)a; (void)f; (void)n; return -30; }
int archive_read_next_header(void *a, void **e) { (void)a; (void)e; return -30; }
long archive_read_data(void *a, void *b, size_t n) { (void)a; (void)b; (void)n; return -30; }
const char *archive_entry_pathname(void *e) { (void)e; return NULL; }
long archive_entry_size(void *e) { (void)e; return 0; }
