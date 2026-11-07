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
    ULONGLONG tick = GetTickCount64();
    DWORD count = 0;
    if (expired || tick >= deadline) return -1;
    DWORD wait = (DWORD)((deadline-tick) < 5000 ? deadline-tick : 5000);
    if (WaitForSingleObject(input, wait) != WAIT_OBJECT_0) return -1;
    return ReadFile(input, out, 1, &count, NULL) && count == 1 ? 0 : -1;
}

static int line(char *out, size_t capacity) {
    for (size_t n=0; n+1<capacity; ++n) {
        unsigned char c;
        if (byte(&c)) return -1;
        if (c=='\n' || c=='\r') {out[n]=0; return 0;}
        out[n]=(char)c;
    }
    return -1;
}

static int query(const char *token, int phase) {
    const unsigned char expected[] = "\033P>|libghostty\033\\";
    unsigned char reply[sizeof(expected)-1];
    FILETIME query_time;
    GetSystemTimePreciseAsFileTime(&query_time);
    ULONGLONG query_filetime = ((ULONGLONG)query_time.dwHighDateTime << 32) | query_time.dwLowDateTime;
    /* Fixed Ghostty stream_terminal.zig reportXtversion defaults to libghostty.
     * CPR is answered inside ConPTY and cannot prove this engine path. */
    fputs("\033[>0q", stdout);
    for (size_t n=0; n<sizeof(reply); ++n) if (byte(&reply[n])) return -1;
    if (memcmp(reply, expected, sizeof(reply))) return -1;
    printf("{\"event\":\"query_reply\",\"token\":\"%s\",\"phase\":%d,\"query\":\"XTVERSION\",\"query_filetime\":%llu,\"reply_hex\":\"1b503e7c6c696267686f737474791b5c\"}\n",token,phase,query_filetime);
    printf("{\"event\":\"continuation\",\"token\":\"%s\",\"phase\":%d}\n",token,phase);
    return 0;
}

int main(int argc, char **argv) {
    const char *alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    if (argc!=2 || strlen(argv[1])<8 || strlen(argv[1])>32 ||
        strspn(argv[1],alphabet)!=strlen(argv[1])) return 64;
    input=GetStdHandle(STD_INPUT_HANDLE);
    HANDLE output=GetStdHandle(STD_OUTPUT_HANDLE);
    DWORD saved_input, saved_output;
    if (!GetConsoleMode(input,&saved_input) || !GetConsoleMode(output,&saved_output)) return 65;
    if (!SetConsoleMode(input,ENABLE_EXTENDED_FLAGS|ENABLE_VIRTUAL_TERMINAL_INPUT)) return 66;
    if (!SetConsoleMode(output,saved_output|ENABLE_PROCESSED_OUTPUT|ENABLE_VIRTUAL_TERMINAL_PROCESSING)) {
        SetConsoleMode(input,saved_input); return 67;
    }
    setvbuf(stdout,NULL,_IONBF,0);
    main_thread=OpenThread(THREAD_TERMINATE,FALSE,GetCurrentThreadId());
    done=CreateEventW(NULL,TRUE,FALSE,NULL);
    if (!main_thread || !done) {
        if (main_thread) CloseHandle(main_thread);
        if (done) CloseHandle(done);
        SetConsoleMode(input,saved_input); SetConsoleMode(output,saved_output); return 68;
    }
    deadline=GetTickCount64()+60000;
    HANDLE timer=CreateThread(NULL,0,watchdog,NULL,0,NULL);
    if (!timer) {
        CloseHandle(main_thread); CloseHandle(done);
        SetConsoleMode(input,saved_input); SetConsoleMode(output,saved_output); return 68;
    }
    FILETIME created, exited, kernel, user;
    if (!GetProcessTimes(GetCurrentProcess(),&created,&exited,&kernel,&user)) {
        created.dwHighDateTime=0; created.dwLowDateTime=0;
    }
    ULONGLONG creation=((ULONGLONG)created.dwHighDateTime<<32)|created.dwLowDateTime;
    printf("{\"event\":\"ready\",\"token\":\"%s\",\"pid\":%lu,\"creation_filetime\":%llu,\"tty\":true,\"input_echo\":false}\n",argv[1],GetCurrentProcessId(),creation);
    int code=69;
    for (int command=0; command<16; ++command) {
        char text[128];
        if (line(text,sizeof(text))) break;
        if (!strcmp(text,"QUERY1") || !strcmp(text,"QUERY2")) {
            if (query(argv[1],text[5]-'0')) break;
        } else if (!strcmp(text,"DETACHQUERY2")) {
            printf("{\"event\":\"detach_armed\",\"token\":\"%s\",\"delay_ms\":3000}\n",argv[1]);
            Sleep(3000);
            if (query(argv[1],2)) break;
        } else if (!strncmp(text,"MARK ",5)) {
            if (!text[5] || strspn(text+5,alphabet)!=strlen(text+5)) break;
            /* Response prefix never appears in the injected input. */
            printf("{\"event\":\"marker_ack\",\"token\":\"%s\",\"value\":\"%s\"}\n",argv[1],text+5);
        } else if (!strcmp(text,"SIZE")) {
            CONSOLE_SCREEN_BUFFER_INFO size;
            if (!GetConsoleScreenBufferInfo(output,&size)) break;
            printf("{\"event\":\"size\",\"token\":\"%s\",\"rows\":%d,\"cols\":%d,\"window_rows\":%d,\"window_cols\":%d}\n",argv[1],size.dwSize.Y,size.dwSize.X,size.srWindow.Bottom-size.srWindow.Top+1,size.srWindow.Right-size.srWindow.Left+1);
        } else if (!strcmp(text,"EXIT0") || !strcmp(text,"EXIT7")) {
            code=text[4]-'0';
            printf("{\"event\":\"tail\",\"token\":\"%s\",\"code\":%d}\n",argv[1],code);
            break;
        } else break;
    }
    if (code==69) printf("{\"event\":\"fixture_error\",\"token\":\"%s\",\"timeout\":%s,\"win32_error\":%lu}\n",argv[1],expired?"true":"false",GetLastError());
    SetEvent(done);
    DWORD timer_exit=WaitForSingleObject(timer,1000);
    if (timer_exit==WAIT_OBJECT_0) {CloseHandle(done); CloseHandle(main_thread);}
    CloseHandle(timer);
    if (!SetConsoleMode(input,saved_input) || !SetConsoleMode(output,saved_output)) return 70;
    return code;
}

