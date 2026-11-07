/* Controlled Window terminal-query fixture, derived from Loomterm M1.
 * Never creates a PTY, process, network connection or file. Run through a real
 * Shell reached through the interactive Loomterm TUI. */
#define WIN32_LEAN_AND_MEAN
#define _WIN32_WINNT 0x0A00
#include <windows.h>
#include <stdio.h>
#include <string.h>

static HANDLE input, main_thread, done;
static ULONGLONG deadline;
static volatile LONG expired;

static DWORD WINAPI watchdog(LPVOID unused) {
    (void)unused;
    if (WaitForSingleObject(done, 60000) == WAIT_TIMEOUT) {
        InterlockedExchange(&expired, 1);
        CancelSynchronousIo(main_thread);
    }
    return 0;
}

static int byte(unsigned char *out) {
    static unsigned char pending[64];
    static DWORD next, used;
    if (next < used) { *out=pending[next++]; return 0; }
    ULONGLONG tick = GetTickCount64();
    if (expired || tick >= deadline) return -1;
    /* ReadFile may retain UTF-8 continuation bytes after consuming the last
     * INPUT_RECORD. Waiting on the console handle again would strand them.
     * The owned watchdog cancels this exact thread's synchronous read. */
    /* Keep room for a complete UTF-8 scalar; byte-buffering does not establish
     * whether ConPTY has paired the underlying KEY_EVENT_RECORD units. */
    next=0; used=0;
    if (!ReadFile(input,pending,sizeof(pending),&used,NULL) || !used) return -1;
    *out=pending[next++];
    return 0;
}

int main(int argc, char **argv) {
    const char *alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    if (argc!=2 || strlen(argv[1])<8 || strlen(argv[1])>32 ||
        strspn(argv[1],alphabet)!=strlen(argv[1])) return 64;
    input=GetStdHandle(STD_INPUT_HANDLE);
    HANDLE output=GetStdHandle(STD_OUTPUT_HANDLE);
    DWORD saved_input, saved_output;
    UINT saved_cp=GetConsoleCP();
    if (!GetConsoleMode(input,&saved_input) || !GetConsoleMode(output,&saved_output)) return 65;
    if (!SetConsoleMode(input,ENABLE_EXTENDED_FLAGS|ENABLE_VIRTUAL_TERMINAL_INPUT)) return 66;
    if (!SetConsoleMode(output,saved_output|ENABLE_PROCESSED_OUTPUT|ENABLE_VIRTUAL_TERMINAL_PROCESSING)) return 67;
    if (!SetConsoleCP(CP_UTF8) || GetConsoleCP()!=CP_UTF8) {
        SetConsoleMode(input,saved_input); SetConsoleMode(output,saved_output);
        SetConsoleCP(saved_cp); return 70;
    }
    DWORD effective_mode;
    if (!GetConsoleMode(input,&effective_mode) ||
        effective_mode!=(ENABLE_EXTENDED_FLAGS|ENABLE_VIRTUAL_TERMINAL_INPUT)) {
        SetConsoleMode(input,saved_input); SetConsoleMode(output,saved_output);
        SetConsoleCP(saved_cp); return 71;
    }
    setvbuf(stdout,NULL,_IONBF,0);
    main_thread=OpenThread(THREAD_TERMINATE,FALSE,GetCurrentThreadId());
    done=CreateEventW(NULL,TRUE,FALSE,NULL);
    HANDLE guard=CreateThread(NULL,0,watchdog,NULL,0,NULL);
    if (!main_thread || !done || !guard) return 68;
    deadline=GetTickCount64()+60000;
    fputs("\033[2J\033[H\033[?2004h",stdout);
    printf("M3_INPUT_READY_%s\r\n",argv[1]);
    printf("M3_INPUT_CP_%u_MODE_%08lx\r\n",GetConsoleCP(),effective_mode);
    int result=0;
    for (unsigned int i=0; i<256; ++i) {
        unsigned char value;
        if (byte(&value)) { result=69; break; }
        if (value==4) break;
        printf("M3_BYTE_%02x\r\n",value);
    }
    fputs("\033[?2004l\r\nM3_INPUT_DONE\r\n",stdout);
    SetConsoleMode(input,saved_input); SetConsoleMode(output,saved_output); SetConsoleCP(saved_cp);
    SetEvent(done); WaitForSingleObject(guard,1000);
    CloseHandle(guard); CloseHandle(done); CloseHandle(main_thread);
    return result;
}
