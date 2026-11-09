/* Loomterm M2: bounded child fixture; creates no PTY or backend itself. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/select.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

static double deadline;
static double now(void) {
    struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}
static long long realtime_ns(void) {
    struct timespec t; clock_gettime(CLOCK_REALTIME, &t);
    return (long long)t.tv_sec * 1000000000LL + t.tv_nsec;
}
static int byte(unsigned char *out) {
    double left = deadline - now(); if (left <= 0) return -1;
    if (left > 5) left = 5;
    struct timeval timeout = {(long)left, (long)((left - (long)left)*1e6)};
    fd_set set; FD_ZERO(&set); FD_SET(0, &set);
    int result = select(1, &set, NULL, NULL, &timeout);
    if (result < 0 && errno == EINTR) return byte(out);
    return result == 1 && read(0, out, 1) == 1 ? 0 : -1;
}
static int line(char *text, size_t capacity) {
    for (size_t n=0; n+1<capacity; ++n) {
        unsigned char c; if (byte(&c)) return -1;
        if (c=='\n' || c=='\r') { text[n]=0; return 0; }
        text[n]=(char)c;
    }
    return -1;
}
static int delay(unsigned seconds) {
    if (deadline-now()<seconds) return -1;
    struct timespec remaining={(time_t)seconds,0};
    while (nanosleep(&remaining,&remaining)) if (errno!=EINTR || now()>=deadline) return -1;
    return 0;
}
static int query(const char *token, int phase) {
    static const unsigned char expected[]="\033P>|libghostty\033\\";
    unsigned char reply[32]; size_t n=0; long long sent=realtime_ns();
    fputs("\033[>0q",stdout); fflush(stdout);
    do {
        if (n==sizeof(reply) || byte(&reply[n])) return -1;
        ++n;
    } while (n<2 || reply[n-2]!=27 || reply[n-1]!='\\');
    if (n!=sizeof(expected)-1 || memcmp(reply,expected,n)) return -1;
    printf("{\"event\":\"query_reply\",\"token\":\"%s\",\"phase\":%d,\"query_time_ns\":%lld,\"reply_time_ns\":%lld,\"reply\":\"libghostty\"}\n",token,phase,sent,realtime_ns());
    printf("{\"event\":\"continuation\",\"token\":\"%s\",\"phase\":%d}\n",token,phase); fflush(stdout); return 0;
}
int main(int argc,char **argv) {
    if (argc!=2 || strlen(argv[1])<8 || strlen(argv[1])>32 || strspn(argv[1],"abcdefghijklmnopqrstuvwxyz0123456789")!=strlen(argv[1])) return 64;
    if (!isatty(0) || !isatty(1)) return 65;
    struct termios saved,raw; if (tcgetattr(0,&saved)) return 66;
    raw=saved; raw.c_lflag &= ~(ICANON|ECHO); raw.c_iflag &= ~(ICRNL|INLCR|IXON); raw.c_cc[VMIN]=1; raw.c_cc[VTIME]=0;
    if (tcsetattr(0,TCSANOW,&raw)) return 67;
    deadline=now()+60;
    printf("{\"event\":\"ready\",\"token\":\"%s\",\"pid\":%ld,\"pgrp\":%ld,\"sid\":%ld,\"tty\":true}\n",argv[1],(long)getpid(),(long)getpgrp(),(long)getsid(0)); fflush(stdout);
    int code=68;
    for (int command=0;command<16;++command) {
        char input[128]; if (line(input,sizeof(input))) break;
        if (!strcmp(input,"QUERY1")) { if (query(argv[1],1)) break; }
        else if (!strcmp(input,"DETACHQUERY2")) {
            printf("{\"event\":\"detach_armed\",\"token\":\"%s\",\"delay_ms\":3000}\n",argv[1]); fflush(stdout);
            if (delay(3) || query(argv[1],2)) break;
        } else if (!strncmp(input,"MARK ",5)) {
            if (strspn(input+5,"abcdefghijklmnopqrstuvwxyz0123456789")!=strlen(input+5)) break;
            printf("{\"event\":\"marker_ack\",\"token\":\"%s\",\"value\":\"%s\"}\n",argv[1],input+5); fflush(stdout);
        } else if (!strcmp(input,"SIZE")) {
            struct winsize size; if (ioctl(0,TIOCGWINSZ,&size)) break;
            printf("{\"event\":\"size\",\"token\":\"%s\",\"rows\":%u,\"cols\":%u}\n",argv[1],size.ws_row,size.ws_col); fflush(stdout);
        } else if (!strcmp(input,"EXIT0") || !strcmp(input,"EXIT7")) {
            code=input[4]-'0'; printf("{\"event\":\"tail\",\"token\":\"%s\",\"code\":%d}\n",argv[1],code); fflush(stdout); break;
        } else if (!strcmp(input,"SIGNAL")) {
            printf("{\"event\":\"signal_tail\",\"token\":\"%s\",\"signal\":%d}\n",argv[1],SIGTERM); fflush(stdout);
            tcsetattr(0,TCSANOW,&saved); raise(SIGTERM); break;
        } else if (!strcmp(input,"HOLD")) {
            pid_t child=fork(); if (child<0) break;
            if (!child) { if (!delay(3)) { printf("{\"event\":\"descendant_tail\",\"token\":\"%s\"}\n",argv[1]); fflush(stdout); } _exit(0); }
            printf("{\"event\":\"descendant\",\"token\":\"%s\",\"pid\":%ld,\"pgrp\":%ld,\"sid\":%ld}\n",argv[1],(long)child,(long)getpgrp(),(long)getsid(0)); fflush(stdout); code=0; break;
        } else break;
    }
    tcsetattr(0,TCSANOW,&saved); return code;
}
