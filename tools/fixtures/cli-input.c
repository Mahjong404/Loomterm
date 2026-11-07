/* Loomterm Windows input conversion control. Synthetic data only; no files,
 * Shell, Agent, network or child processes. The owning runtime supplies a PTY.
 * Three APIs observe the same CP/mode and exact UTF-8 input, without a TUI. */
#define WIN32_LEAN_AND_MEAN
#define _WIN32_WINNT 0x0A00
#include <windows.h>
#include <stdio.h>
#include <string.h>

static HANDLE done, reader;
static DWORD WINAPI watchdog(LPVOID unused) {
    (void)unused;
    if (WaitForSingleObject(done, 15000) == WAIT_TIMEOUT) CancelSynchronousIo(reader);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 2 || (strcmp(argv[1], "bytes") && strcmp(argv[1], "wide") &&
                      strcmp(argv[1], "records"))) return 64;
    HANDLE input = GetStdHandle(STD_INPUT_HANDLE);
    DWORD saved_mode, mode;
    UINT saved_cp = GetConsoleCP();
    if (!GetConsoleMode(input, &saved_mode)) return 65;
    if (!SetConsoleMode(input, ENABLE_EXTENDED_FLAGS | ENABLE_VIRTUAL_TERMINAL_INPUT) ||
        !SetConsoleCP(CP_UTF8) || GetConsoleCP() != CP_UTF8 ||
        !GetConsoleMode(input, &mode) || mode != 0x280) return 66;
    reader = OpenThread(THREAD_TERMINATE, FALSE, GetCurrentThreadId());
    done = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (!reader || !done) return 67;
    HANDLE guard = CreateThread(NULL, 0, watchdog, NULL, 0, NULL);
    if (!guard) return 68;
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("CLI_INPUT_READY_%s_CP%u_MODE%08lx\r\n", argv[1], GetConsoleCP(), mode);
    int result = 0, finished = 0;
    for (unsigned int budget = 0; budget < 256 && !finished; ++budget) {
        DWORD count = 0;
        if (!strcmp(argv[1], "bytes")) {
            unsigned char buffer[64];
            if (!ReadFile(input, buffer, sizeof(buffer), &count, NULL) || !count) { result = 69; break; }
            for (DWORD i = 0; i < count; ++i) {
                if (buffer[i] == 4) { finished = 1; break; }
                printf("CLI_B_%02x\r\n", buffer[i]);
            }
        } else if (!strcmp(argv[1], "wide")) {
            WCHAR buffer[64];
            if (!ReadConsoleW(input, buffer, 64, &count, NULL) || !count) { result = 70; break; }
            for (DWORD i = 0; i < count; ++i) {
                if (buffer[i] == 4) { finished = 1; break; }
                printf("CLI_W_%04x\r\n", (unsigned int)buffer[i]);
            }
        } else {
            INPUT_RECORD buffer[64];
            if (!ReadConsoleInputW(input, buffer, 64, &count) || !count) { result = 71; break; }
            for (DWORD i = 0; i < count; ++i) {
                if (buffer[i].EventType != KEY_EVENT) continue;
                WCHAR unit = buffer[i].Event.KeyEvent.uChar.UnicodeChar;
                printf("CLI_RECORD_down%ld_vk%u_unit%04x\r\n",
                    buffer[i].Event.KeyEvent.bKeyDown,
                    (unsigned int)buffer[i].Event.KeyEvent.wVirtualKeyCode,
                    (unsigned int)unit);
                /* System ConPTY can submit Alt-code text on key release. */
                if (!buffer[i].Event.KeyEvent.bKeyDown &&
                    buffer[i].Event.KeyEvent.wVirtualKeyCode != VK_MENU) continue;
                if (unit == 4) { finished = 1; break; }
                if (unit) printf("CLI_R_%04x\r\n", (unsigned int)unit);
            }
        }
    }
    if (!finished && !result) result = 72;
    printf("CLI_INPUT_DONE_%d\r\n", result);
    SetConsoleMode(input, saved_mode);
    SetConsoleCP(saved_cp);
    SetEvent(done);
    WaitForSingleObject(guard, 1000);
    CloseHandle(guard); CloseHandle(done); CloseHandle(reader);
    return result;
}
