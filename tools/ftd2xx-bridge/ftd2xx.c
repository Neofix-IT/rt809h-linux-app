/*
 * Research stand-in for ftd2xx.dll: implements the public D2XX API by
 * forwarding each call over TCP to the `bridge` example of rt809h-core,
 * which drives the real FT2232H from Linux and logs all traffic.
 * Used only to observe the programmer's wire protocol.
 */
#include <winsock2.h>
#include <windows.h>
#include <stdio.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

typedef ULONG FT_STATUS;
typedef PVOID FT_HANDLE;
#define FT_OK 0
#define FT_INVALID_HANDLE 1
#define FT_DEVICE_NOT_FOUND 2
#define FT_IO_ERROR 4
#define FT_NOT_SUPPORTED 17

#define EXPORT __declspec(dllexport)

static SOCKET g_sock = INVALID_SOCKET;
static CRITICAL_SECTION g_lock;
static DWORD g_read_timeout[2] = {5000, 5000};
static char g_serial[32] = "gggggggg";
static char g_desc[64] = "Mstar USB Debug Tool";
static int g_listed = 0;

static void flog(const char *b);

static int xfer_all(int send_dir, char *buf, int len) {
    while (len > 0) {
        int n = send_dir ? send(g_sock, buf, len, 0) : recv(g_sock, buf, len, 0);
        if (n <= 0) return -1;
        buf += n; len -= n;
    }
    return 0;
}

static int connect_bridge(void) {
    if (g_sock != INVALID_SOCKET) return 0;
    WSADATA w; WSAStartup(MAKEWORD(2, 2), &w);
    g_sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    struct sockaddr_in a; memset(&a, 0, sizeof a);
    a.sin_family = AF_INET; a.sin_port = htons(9809);
    a.sin_addr.s_addr = inet_addr("127.0.0.1");
    if (connect(g_sock, (struct sockaddr *)&a, sizeof a) != 0) {
        closesocket(g_sock); g_sock = INVALID_SOCKET; return -1;
    }
    int one = 1; setsockopt(g_sock, IPPROTO_TCP, TCP_NODELAY, (char *)&one, sizeof one);
    return 0;
}

/* returns status; *out receives up to outcap bytes, *outlen actual */
static FT_STATUS rpc(int op, int ch, const void *p, DWORD plen, void *out, DWORD outcap, DWORD *outlen) {
    FT_STATUS st = FT_IO_ERROR;
    if (op != 0x10) { char b[64]; sprintf(b, "rpc op=%d ch=%d len=%lu", op, ch, plen); flog(b); }
    EnterCriticalSection(&g_lock);
    if (connect_bridge() == 0) {
        unsigned char hdr[6]; hdr[0] = (unsigned char)op; hdr[1] = (unsigned char)ch;
        memcpy(hdr + 2, &plen, 4);
        if (xfer_all(1, (char *)hdr, 6) == 0 && (plen == 0 || xfer_all(1, (char *)p, plen) == 0)) {
            DWORD rh[2];
            if (xfer_all(0, (char *)rh, 8) == 0) {
                st = rh[0];
                DWORD n = rh[1];
                char *tmp = (char *)malloc(n ? n : 1);
                if (xfer_all(0, tmp, n) == 0) {
                    DWORD c = n < outcap ? n : outcap;
                    if (out && c) memcpy(out, tmp, c);
                    if (outlen) *outlen = c;
                } else st = FT_IO_ERROR;
                free(tmp);
            }
        }
    }
    LeaveCriticalSection(&g_lock);
    return st;
}

static void flog(const char *b) {
    char *path = getenv("FTD2XX_BRIDGE_LOG");
    if (!path) return;
    FILE *f = fopen(path, "a");
    if (f) { fprintf(f, "%lu %s\n", GetTickCount(), b); fclose(f); }
}

static void note(int ch, const char *fmt, ...) {
    char b[512]; va_list ap; va_start(ap, fmt); vsnprintf(b, sizeof b, fmt, ap); va_end(ap);
    flog(b);
    rpc(0x10, ch, b, (DWORD)strlen(b), NULL, 0, NULL);
}

static void refresh_list(void) {
    if (g_listed) return;
    unsigned char b[128] = {0}; DWORD n = 0;
    if (rpc(1, 0, NULL, 0, b, sizeof b - 1, &n) == FT_OK && n > 1 && b[0] > 0) {
        strncpy(g_serial, (char *)b + 1, sizeof g_serial - 1);
        strncpy(g_desc, (char *)b + 2 + strlen((char *)b + 1), sizeof g_desc - 1);
    }
    g_listed = 1;
}

#define CH(h) ((int)(INT_PTR)(h) - 1)
#define VALID(h) ((INT_PTR)(h) == 1 || (INT_PTR)(h) == 2)

static void chan_strings(int ch, char *serial, char *desc) {
    refresh_list();
    if (serial) sprintf(serial, "%s%c", g_serial, 'A' + ch);
    if (desc) sprintf(desc, "%s %c", g_desc, 'A' + ch);
}

static FT_STATUS do_open(int ch, FT_HANDLE *h) {
    FT_STATUS s = rpc(2, ch, NULL, 0, NULL, 0, NULL);
    if (s == FT_OK) *h = (FT_HANDLE)(INT_PTR)(ch + 1);
    return s;
}

BOOL WINAPI DllMain(HINSTANCE i, DWORD r, LPVOID x) {
    if (r == DLL_PROCESS_ATTACH) { InitializeCriticalSection(&g_lock); flog("attach"); }
    return TRUE;
}

EXPORT FT_STATUS WINAPI FT_CreateDeviceInfoList(LPDWORD n) { flog("FT_CreateDeviceInfoList"); refresh_list(); note(0, "FT_CreateDeviceInfoList"); *n = 2; return FT_OK; }

typedef struct { ULONG Flags, Type, ID; DWORD LocId; char SerialNumber[16]; char Description[64]; FT_HANDLE ftHandle; } NODE;

EXPORT FT_STATUS WINAPI FT_GetDeviceInfoList(NODE *list, LPDWORD n) { flog("FT_GetDeviceInfoList");
    note(0, "FT_GetDeviceInfoList");
    for (int c = 0; c < 2; c++) {
        memset(&list[c], 0, sizeof(NODE));
        list[c].Flags = 2; list[c].Type = 6; list[c].ID = 0x04036010; list[c].LocId = 0x21 + c;
        chan_strings(c, list[c].SerialNumber, list[c].Description);
    }
    *n = 2; return FT_OK;
}

EXPORT FT_STATUS WINAPI FT_GetDeviceInfoDetail(DWORD idx, LPDWORD flags, LPDWORD type, LPDWORD id, LPDWORD loc, LPVOID serial, LPVOID desc, FT_HANDLE *h) { flog("FT_GetDeviceInfoDetail");
    note(0, "FT_GetDeviceInfoDetail %lu", idx);
    if (idx > 1) return FT_DEVICE_NOT_FOUND;
    if (flags) *flags = 2; if (type) *type = 6; if (id) *id = 0x04036010; if (loc) *loc = 0x21 + idx;
    chan_strings(idx, (char *)serial, (char *)desc);
    if (h) *h = NULL;
    return FT_OK;
}

EXPORT FT_STATUS WINAPI FT_ListDevices(PVOID a1, PVOID a2, DWORD flags) { flog("FT_ListDevices");
    note(0, "FT_ListDevices flags=0x%lx", flags);
    if (flags & 0x80000000) { *(DWORD *)a1 = 2; return FT_OK; }
    if (flags & 0x40000000) {
        DWORD i = (DWORD)(INT_PTR)a1; if (i > 1) return FT_DEVICE_NOT_FOUND;
        if ((flags & 0xff) == 2) chan_strings(i, NULL, (char *)a2); else chan_strings(i, (char *)a2, NULL);
        return FT_OK;
    }
    if (flags & 0x20000000) {
        char **arr = (char **)a1;
        for (int c = 0; c < 2; c++) {
            if ((flags & 0xff) == 2) chan_strings(c, NULL, arr[c]); else chan_strings(c, arr[c], NULL);
        }
        if (a2) *(DWORD *)a2 = 2;
        return FT_OK;
    }
    return FT_NOT_SUPPORTED;
}

EXPORT FT_STATUS WINAPI FT_Open(int idx, FT_HANDLE *h) { flog("FT_Open"); note(idx, "FT_Open %d", idx); if (idx < 0 || idx > 1) return FT_DEVICE_NOT_FOUND; return do_open(idx, h); }

EXPORT FT_STATUS WINAPI FT_OpenEx(PVOID arg, DWORD flags, FT_HANDLE *h) { flog("FT_OpenEx");
    char s[80], d[80];
    note(0, "FT_OpenEx flags=%lu arg=%s", flags, (flags & 3) ? (char *)arg : "(loc)");
    for (int c = 0; c < 2; c++) {
        chan_strings(c, s, d);
        if ((flags == 1 && !strcmp(s, (char *)arg)) || (flags == 2 && !strcmp(d, (char *)arg)) ||
            (flags == 4 && (DWORD)(INT_PTR)arg == 0x21 + (DWORD)c))
            return do_open(c, h);
    }
    return FT_DEVICE_NOT_FOUND;
}

EXPORT FT_STATUS WINAPI FT_Close(FT_HANDLE h) { flog("FT_Close"); if (!VALID(h)) return FT_INVALID_HANDLE; return rpc(3, CH(h), NULL, 0, NULL, 0, NULL); }
EXPORT FT_STATUS WINAPI FT_ResetDevice(FT_HANDLE h) { flog("FT_ResetDevice"); if (!VALID(h)) return FT_INVALID_HANDLE; return rpc(4, CH(h), NULL, 0, NULL, 0, NULL); }
EXPORT FT_STATUS WINAPI FT_ResetPort(FT_HANDLE h) { flog("FT_ResetPort"); note(CH(h), "FT_ResetPort"); return FT_OK; }
EXPORT FT_STATUS WINAPI FT_CyclePort(FT_HANDLE h) { flog("FT_CyclePort"); note(CH(h), "FT_CyclePort"); return FT_OK; }
EXPORT FT_STATUS WINAPI FT_Purge(FT_HANDLE h, ULONG m) { flog("FT_Purge"); if (!VALID(h)) return FT_INVALID_HANDLE; return rpc(5, CH(h), &m, 4, NULL, 0, NULL); }
EXPORT FT_STATUS WINAPI FT_SetBitMode(FT_HANDLE h, UCHAR mask, UCHAR mode) { flog("FT_SetBitMode"); unsigned char p[2] = {mask, mode}; if (!VALID(h)) return FT_INVALID_HANDLE; return rpc(6, CH(h), p, 2, NULL, 0, NULL); }
EXPORT FT_STATUS WINAPI FT_GetBitMode(FT_HANDLE h, PUCHAR v) { flog("FT_GetBitMode"); DWORD n; if (!VALID(h)) return FT_INVALID_HANDLE; return rpc(11, CH(h), NULL, 0, v, 1, &n); }
EXPORT FT_STATUS WINAPI FT_SetLatencyTimer(FT_HANDLE h, UCHAR t) { flog("FT_SetLatencyTimer"); if (!VALID(h)) return FT_INVALID_HANDLE; return rpc(7, CH(h), &t, 1, NULL, 0, NULL); }
EXPORT FT_STATUS WINAPI FT_GetLatencyTimer(FT_HANDLE h, PUCHAR t) { flog("FT_GetLatencyTimer"); *t = 2; return FT_OK; }
EXPORT FT_STATUS WINAPI FT_SetTimeouts(FT_HANDLE h, ULONG rd, ULONG wr) { flog("FT_SetTimeouts"); if (!VALID(h)) return FT_INVALID_HANDLE; g_read_timeout[CH(h)] = rd ? rd : 5000; note(CH(h), "FT_SetTimeouts %lu %lu", rd, wr); return FT_OK; }

EXPORT FT_STATUS WINAPI FT_Write(FT_HANDLE h, LPVOID buf, DWORD n, LPDWORD written) { flog("FT_Write");
    DWORD w = 0, got;
    if (!VALID(h)) return FT_INVALID_HANDLE;
    FT_STATUS s = rpc(8, CH(h), buf, n, &w, 4, &got);
    if (written) *written = (s == FT_OK) ? n : 0;
    return s;
}

EXPORT FT_STATUS WINAPI FT_Read(FT_HANDLE h, LPVOID buf, DWORD n, LPDWORD rd) { flog("FT_Read");
    DWORD p[2] = {n, g_read_timeout[CH(h)]}, got = 0;
    if (!VALID(h)) return FT_INVALID_HANDLE;
    FT_STATUS s = rpc(9, CH(h), p, 8, buf, n, &got);
    if (rd) *rd = got;
    return s;
}

EXPORT FT_STATUS WINAPI FT_GetQueueStatus(FT_HANDLE h, LPDWORD n) { flog("FT_GetQueueStatus"); DWORD got; if (!VALID(h)) return FT_INVALID_HANDLE; return rpc(10, CH(h), NULL, 0, n, 4, &got); }
EXPORT FT_STATUS WINAPI FT_GetQueueStatusEx(FT_HANDLE h, LPDWORD n) { flog("FT_GetQueueStatusEx"); return FT_GetQueueStatus(h, n); }
EXPORT FT_STATUS WINAPI FT_GetStatus(FT_HANDLE h, LPDWORD rx, LPDWORD tx, LPDWORD ev) { flog("FT_GetStatus"); if (tx) *tx = 0; if (ev) *ev = 0; return FT_GetQueueStatus(h, rx); }

EXPORT FT_STATUS WINAPI FT_GetDeviceInfo(FT_HANDLE h, ULONG *type, LPDWORD id, PCHAR serial, PCHAR desc, LPVOID dummy) { flog("FT_GetDeviceInfo");
    note(CH(h), "FT_GetDeviceInfo");
    if (type) *type = 6; if (id) *id = 0x04036010;
    chan_strings(CH(h), serial, desc);
    return FT_OK;
}

EXPORT FT_STATUS WINAPI FT_ReadEE(FT_HANDLE h, DWORD off, LPWORD v) { flog("FT_ReadEE");
    DWORD got; WORD w = 0; FT_STATUS s = rpc(12, CH(h), &off, 4, &w, 2, &got); *v = w; return s;
}

EXPORT FT_STATUS WINAPI FT_GetLibraryVersion(LPDWORD v) { flog("FT_GetLibraryVersion"); *v = 0x00030206; return FT_OK; }
EXPORT FT_STATUS WINAPI FT_GetDriverVersion(FT_HANDLE h, LPDWORD v) { flog("FT_GetDriverVersion"); *v = 0x00021204; return FT_OK; }
EXPORT FT_STATUS WINAPI FT_GetComPortNumber(FT_HANDLE h, LPLONG v) { flog("FT_GetComPortNumber"); *v = -1; return FT_OK; }
EXPORT FT_STATUS WINAPI FT_GetModemStatus(FT_HANDLE h, ULONG *v) { flog("FT_GetModemStatus"); *v = 0; return FT_OK; }
EXPORT FT_STATUS WINAPI FT_GetEventStatus(FT_HANDLE h, LPDWORD v) { flog("FT_GetEventStatus"); *v = 0; return FT_OK; }
EXPORT FT_STATUS WINAPI FT_Rescan(void) { flog("FT_Rescan"); note(0, "FT_Rescan"); return FT_OK; }
EXPORT FT_STATUS WINAPI FT_Reload(WORD v, WORD p) { flog("FT_Reload"); return FT_OK; }

#define LOGONLY(name, args, fmt, ...) EXPORT FT_STATUS WINAPI name args { flog(#name); note(0, #name " " fmt, ##__VA_ARGS__); return FT_OK; }
LOGONLY(FT_SetUSBParameters, (FT_HANDLE h, ULONG a, ULONG b), "%lu %lu", a, b)
LOGONLY(FT_SetChars, (FT_HANDLE h, UCHAR a, UCHAR b, UCHAR c, UCHAR d), "%u %u %u %u", a, b, c, d)
LOGONLY(FT_SetFlowControl, (FT_HANDLE h, USHORT f, UCHAR x, UCHAR y), "0x%x", f)
LOGONLY(FT_SetBaudRate, (FT_HANDLE h, ULONG b), "%lu", b)
LOGONLY(FT_SetDivisor, (FT_HANDLE h, USHORT d), "%u", d)
LOGONLY(FT_SetDataCharacteristics, (FT_HANDLE h, UCHAR a, UCHAR b, UCHAR c), "%u %u %u", a, b, c)
LOGONLY(FT_SetDtr, (FT_HANDLE h), "")
LOGONLY(FT_ClrDtr, (FT_HANDLE h), "")
LOGONLY(FT_SetRts, (FT_HANDLE h), "")
LOGONLY(FT_ClrRts, (FT_HANDLE h), "")
LOGONLY(FT_SetBreakOn, (FT_HANDLE h), "")
LOGONLY(FT_SetBreakOff, (FT_HANDLE h), "")
LOGONLY(FT_SetWaitMask, (FT_HANDLE h, DWORD m), "%lu", m)
LOGONLY(FT_SetEventNotification, (FT_HANDLE h, DWORD m, PVOID p), "%lu", m)
LOGONLY(FT_SetResetPipeRetryCount, (FT_HANDLE h, DWORD c), "%lu", c)
LOGONLY(FT_SetDeadmanTimeout, (FT_HANDLE h, ULONG t), "%lu", t)
LOGONLY(FT_StopInTask, (FT_HANDLE h), "")
LOGONLY(FT_RestartInTask, (FT_HANDLE h), "")

#define UNSUP(name, args) EXPORT FT_STATUS WINAPI name args { flog(#name); note(0, #name " (unsupported)"); return FT_NOT_SUPPORTED; }
UNSUP(FT_EE_Read, (FT_HANDLE h, PVOID d))
UNSUP(FT_EE_ReadEx, (FT_HANDLE h, PVOID d, PVOID a, PVOID b, PVOID c, PVOID e))
UNSUP(FT_EE_Program, (FT_HANDLE h, PVOID d))
UNSUP(FT_EE_ProgramEx, (FT_HANDLE h, PVOID d, PVOID a, PVOID b, PVOID c, PVOID e))
UNSUP(FT_EEPROM_Read, (FT_HANDLE h, PVOID d, DWORD s, PVOID a, PVOID b, PVOID c, PVOID e))
UNSUP(FT_EEPROM_Program, (FT_HANDLE h, PVOID d, DWORD s, PVOID a, PVOID b, PVOID c, PVOID e))
UNSUP(FT_EE_UASize, (FT_HANDLE h, LPDWORD s))
UNSUP(FT_EE_UARead, (FT_HANDLE h, PUCHAR d, DWORD l, LPDWORD r))
UNSUP(FT_EE_UAWrite, (FT_HANDLE h, PUCHAR d, DWORD l))
UNSUP(FT_EE_ReadConfig, (FT_HANDLE h, UCHAR a, LPBYTE v))
UNSUP(FT_EE_WriteConfig, (FT_HANDLE h, UCHAR a, BYTE v))
UNSUP(FT_EE_ReadECC, (FT_HANDLE h, UCHAR a, LPWORD v))
UNSUP(FT_WriteEE, (FT_HANDLE h, DWORD o, WORD v))
UNSUP(FT_EraseEE, (FT_HANDLE h))
UNSUP(FT_IoCtl, (FT_HANDLE h, DWORD a, LPVOID b, DWORD c, LPVOID d, DWORD e, LPDWORD f, LPVOID g))
UNSUP(FT_WaitOnMask, (FT_HANDLE h, DWORD *m))
