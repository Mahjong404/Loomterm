/* Bounded raw ConPTY paste fixture. Run only in an owned Loomterm work Pane.
 * It prints one digest instead of echoing private input or unbounded byte rows. */
#define WIN32_LEAN_AND_MEAN
#define _WIN32_WINNT 0x0A00
#include <windows.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_BYTES (2u * 1024u * 1024u)
static DWORD timeout_ms = 15000u;

static HANDLE main_thread;
static HANDLE done;
static volatile LONG expired;

static DWORD WINAPI watchdog(LPVOID unused) {
    (void)unused;
    if (WaitForSingleObject(done, timeout_ms) == WAIT_TIMEOUT) {
        InterlockedExchange(&expired, 1);
        CancelSynchronousIo(main_thread);
    }
    return 0;
}

int main(int argc, char **argv) {
    const char *alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    unsigned long max_bytes = MAX_BYTES;
    unsigned long timeout_seconds = 15;
    if (argc < 2 || argc > 4 || strlen(argv[1]) < 8 || strlen(argv[1]) > 32 ||
        strspn(argv[1], alphabet) != strlen(argv[1])) return 64;
    if (argc >= 3) {
        char *end = NULL;
        errno = 0;
        max_bytes = strtoul(argv[2], &end, 10);
        if (errno || !end || *end || max_bytes < 1 || max_bytes > MAX_BYTES) return 64;
    }
    if (argc == 4) {
        char *end = NULL;
        errno = 0;
        timeout_seconds = strtoul(argv[3], &end, 10);
        if (errno || !end || *end || timeout_seconds < 5 || timeout_seconds > 240) return 64;
    }
    timeout_ms = (DWORD)(timeout_seconds * 1000);
    HANDLE input = GetStdHandle(STD_INPUT_HANDLE);
    HANDLE output = GetStdHandle(STD_OUTPUT_HANDLE);
    DWORD saved_input, saved_output;
    UINT saved_cp = GetConsoleCP();
    if (!GetConsoleMode(input, &saved_input) ||
        !GetConsoleMode(output, &saved_output)) return 65;
    if (!SetConsoleMode(input, ENABLE_EXTENDED_FLAGS | ENABLE_VIRTUAL_TERMINAL_INPUT) ||
        !SetConsoleMode(output, saved_output | ENABLE_PROCESSED_OUTPUT |
                                ENABLE_VIRTUAL_TERMINAL_PROCESSING) ||
        !SetConsoleCP(CP_UTF8)) return 66;
    main_thread = OpenThread(THREAD_TERMINATE, FALSE, GetCurrentThreadId());
    done = CreateEventW(NULL, TRUE, FALSE, NULL);
    HANDLE guard = CreateThread(NULL, 0, watchdog, NULL, 0, NULL);
    if (!main_thread || !done || !guard) return 67;
    setvbuf(stdout, NULL, _IONBF, 0);
    fputs("\033[?2004h", stdout);
    printf("M3_PASTE_READY_%s\r\n", argv[1]);
    const unsigned char start[] = "\033[200~";
    const unsigned char end[] = "\033[201~";
    unsigned char tail[sizeof(end) - 1] = {0};
    unsigned int tail_used = 0, start_used = 0, end_used = 0;
    uint64_t hash = UINT64_C(14695981039346656037);
    unsigned int count = 0;
    unsigned char chunk[4096];
    int result = 69;
    DWORD read_error = 0;
    while (!expired && count < max_bytes) {
        DWORD read = 0;
        if (!ReadFile(input, chunk, sizeof(chunk), &read, NULL)) {
            read_error = GetLastError();
            break;
        }
        if (!read) break;
        for (DWORD i = 0; i < read; ++i) {
            unsigned char byte = chunk[i];
            hash = (hash ^ byte) * UINT64_C(1099511628211);
            ++count;
            if (start_used < sizeof(start) - 1) {
                if (byte == start[start_used]) ++start_used;
                else start_used = 0;
            }
            if (tail_used < sizeof(end) - 1) tail[tail_used++] = byte;
            else {
                memmove(tail, tail + 1, sizeof(end) - 2);
                tail[sizeof(end) - 2] = byte;
            }
            if (tail_used == sizeof(end) - 1 &&
                memcmp(tail, end, sizeof(end) - 1) == 0) {
                ++end_used;
                result = 0;
                break;
            }
            if (count == max_bytes) { result = 70; break; }
        }
        if (result == 0 || result == 70) break;
    }
    printf("M3_PASTE_RESULT_%s bytes=%u fnv=%016llx start=%u end=%u rc=%d error=%lu\r\n",
           argv[1], count, (unsigned long long)hash,
           start_used == sizeof(start) - 1, end_used, result, read_error);
    fputs("\033[?2004l", stdout);
    SetConsoleMode(input, saved_input);
    SetConsoleMode(output, saved_output);
    SetConsoleCP(saved_cp);
    SetEvent(done);
    WaitForSingleObject(guard, 1000);
    CloseHandle(guard);
    CloseHandle(done);
    CloseHandle(main_thread);
    return result;
}
