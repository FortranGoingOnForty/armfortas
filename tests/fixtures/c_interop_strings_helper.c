/* Differential C side for the l06 string-interop tests. The Fortran main
 * drives; these helpers hand strings across the boundary and verify the
 * Fortran view against C's own strlen/strcmp. */
#include <string.h>

/* A NUL-terminated C string for the C_F_STRPOINTER c_ptr form. */
const char *afs_test_hello(void) {
    return "hello";
}

/* Embedded NUL: the visible C string is "ab"; bytes after the NUL exist
 * but a strlen / C_F_STRPOINTER scan must stop at the NUL. */
static char afs_embedded[6] = {'a', 'b', '\0', 'c', 'd', '\0'};
const char *afs_test_embedded(void) {
    return afs_embedded;
}

/* C's strlen of whatever pointer Fortran passes back. */
long afs_test_strlen(const char *p) {
    return (long)strlen(p);
}

/* strcmp == 0 → returns 1, else 0. Used to check F_C_STRING output. */
int afs_test_streq(const char *p, const char *expect) {
    return strcmp(p, expect) == 0 ? 1 : 0;
}

/* A mutable buffer "abc\0" for the round-trip aliasing test. */
static char afs_rt[6] = {'a', 'b', 'c', '\0', '\0', '\0'};
const char *afs_test_rtbuf(void) {
    return afs_rt;
}
char afs_test_rt_first(void) {
    return afs_rt[0];
}

/* An integer array [10,20,30,40] for the C_F_POINTER LOWER test. */
static int afs_ints[4] = {10, 20, 30, 40};
const int *afs_test_ints(void) {
    return afs_ints;
}
