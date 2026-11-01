/*
 * rime-term-graphics: which inline-image protocol the terminal on /dev/tty can show, for the Rime OS
 * fastfetch greeting (files/desktop/shell/rime-greeting.sh).
 *
 *   rime-term-graphics                          prints the protocol
 *   rime-term-graphics --rows N [--image PNG]   prints "<protocol> <cols> <rows> <cache-token>"
 *
 * The protocol is one of kitty-direct, kitty, sixel, iterm, none: fastfetch's --logo-type names, plus none,
 * which the greeting turns into a plain "RIME OS" label (never ASCII art). <cols> is the logo's width in
 * cells for N rows, from the image's aspect ratio and the terminal's cell size, so the terminal does not
 * stretch it. fastfetch must get both a width and a height: with only a width it clears the screen and the
 * scrollback before drawing an image logo.
 *
 * The rules, in order:
 *   RIME_TERM_GRAPHICS_FORCE=<protocol>        what you set, unchecked (an escape hatch for odd terminals)
 *   RIME_TERM_GRAPHICS=<protocol>@<key>        a previous answer for this terminal (the greeting exports it)
 *   TERM unset, dumb or linux; no /dev/tty     none
 *   GNU screen                                 none (fastfetch refuses image logos under TERM=screen)
 *   tmux                                       kitty-direct only when tmux's allow-passthrough is on and the
 *                                              outer terminal is a local Ghostty or kitty (known from the
 *                                              environment tmux inherited); otherwise none. A query there
 *                                              would be answered by tmux, not by the terminal.
 *   Alacritty                                  none (it has no image protocol)
 *   local Ghostty or kitty                     kitty-direct (the terminal reads the file itself)
 *   local WezTerm                              iterm (WezTerm turns its kitty protocol off by default; its
 *                                              iTerm2 inline images are always on)
 *   anything else, and every SSH session       ONE round trip: a kitty graphics query, then (only when the
 *                                              cell size is unknown) CSI 14 t, then DA1. DA1 is answered by
 *                                              almost every terminal, so its reply ends the wait early.
 *                                              kitty reply OK -> kitty (fastfetch streams the pixels);
 *                                              DA1 lists 4 -> sixel; LC_TERMINAL / TERM_PROGRAM says iTerm2
 *                                              or WezTerm -> iterm; else none. kitty and sixel also need the
 *                                              cell size in pixels (fastfetch scales with it and, without it,
 *                                              falls back to an ASCII logo), so without one they are none.
 *
 * The query waits at most RIME_TERM_GRAPHICS_TIMEOUT_MS from the start of this process: by default 35 ms for
 * a local terminal and 300 ms over SSH. The wait ends as soon as DA1 arrives, which almost every terminal
 * sends, so a terminal that answers pays its round trip and only a silent one pays the whole deadline. Over
 * SSH the deadline has to cover the link's round trip: a reply that lands after this process has exited
 * cannot be flushed, and the shell would read it as typed input ("62;22c" at the first prompt). A terminal
 * that answers later than the deadline gets the label; raise the timeout for a link slower than that. The tty's
 * settings are restored on every path: normal exit, timeout, and SIGINT/SIGTERM/SIGHUP/SIGQUIT (the signal
 * is then re-raised, so Ctrl-C still interrupts the caller). Ctrl-Z is ignored during the wait.
 *
 * Plain C on purpose: it runs before the first prompt of every terminal, and a Python start alone measured
 * 30-120 ms on a loaded machine, which is the whole budget.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

enum proto { P_NONE, P_KITTY_DIRECT, P_KITTY, P_SIXEL, P_ITERM };
static const char *const proto_name[] = { "none", "kitty-direct", "kitty", "sixel", "iterm" };

/* The Rime symbol's viewBox (files/branding/ridge/templates/rime-symbol.svg.in), used when no image is given
 * or the image is not a PNG. */
#define DEFAULT_IMG_W 240.0
#define DEFAULT_IMG_H 175.0
/* Height / width of a cell when the terminal reports no pixel size: a typical monospace font. */
#define DEFAULT_CELL_RATIO 2.1

static int tty_fd = -1;
static struct termios saved_termios;
static volatile sig_atomic_t termios_saved = 0;
static double start_ms;

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double) ts.tv_sec * 1000.0 + (double) ts.tv_nsec / 1e6;
}

static const char *env(const char *name) {
    const char *v = getenv(name);
    return v ? v : "";
}

static int starts_with(const char *s, const char *prefix) {
    return strncmp(s, prefix, strlen(prefix)) == 0;
}

static void restore_tty(void) {
    if (termios_saved) {
        tcsetattr(tty_fd, TCSANOW, &saved_termios);
        termios_saved = 0;
    }
}

static void on_signal(int sig) {
    restore_tty(); /* tcsetattr is async-signal-safe */
    signal(sig, SIG_DFL);
    raise(sig);
}

static int parse_proto(const char *s) {
    for (int i = 0; i < (int) (sizeof proto_name / sizeof proto_name[0]); i++) {
        if (strcmp(s, proto_name[i]) == 0) {
            return i;
        }
    }
    return -1;
}

/* Identifies this terminal session for the cache: the tty, TERM and the SSH tty, without spaces. */
static void session_key(char *out, size_t size) {
    const char *tty = ttyname(STDIN_FILENO);
    if (!tty) {
        tty = ttyname(STDERR_FILENO);
    }
    snprintf(out, size, "%s,%s,%s", tty ? tty : "-", env("TERM"), *env("SSH_TTY") ? env("SSH_TTY") : "-");
    for (char *p = out; *p; p++) {
        if (*p == ' ' || *p == '\t' || *p == '\n' || *p == '@') {
            *p = '_';
        }
    }
}

static int is_remote(void) {
    return *env("SSH_CONNECTION") || *env("SSH_CLIENT") || *env("SSH_TTY");
}

static int is_ghostty(void) {
    return strcmp(env("TERM"), "xterm-ghostty") == 0 || *env("GHOSTTY_RESOURCES_DIR") ||
           strcmp(env("TERM_PROGRAM"), "ghostty") == 0;
}

static int is_kitty(void) {
    return strcmp(env("TERM"), "xterm-kitty") == 0 || *env("KITTY_WINDOW_ID");
}

static int is_wezterm(void) {
    return strcmp(env("TERM_PROGRAM"), "WezTerm") == 0 || *env("WEZTERM_PANE");
}

static int is_alacritty(void) {
    return strcmp(env("TERM"), "alacritty") == 0 || starts_with(env("TERM"), "alacritty-") ||
           *env("ALACRITTY_WINDOW_ID") || *env("ALACRITTY_SOCKET");
}

static int says_iterm(void) {
    const char *lc = env("LC_TERMINAL"), *tp = env("TERM_PROGRAM");
    return strcmp(lc, "iTerm2") == 0 || strcmp(lc, "WezTerm") == 0 || strcmp(tp, "iTerm.app") == 0 ||
           strcmp(tp, "WezTerm") == 0;
}

/* tmux's allow-passthrough is "on" or "all" (tmux 3.3+). Asking is a local socket round trip. */
static int tmux_passthrough(void) {
    FILE *p = popen("tmux show-options -gv allow-passthrough 2>/dev/null", "r");
    if (!p) {
        return 0;
    }
    char line[32] = "";
    if (!fgets(line, sizeof line, p)) {
        line[0] = '\0';
    }
    pclose(p);
    line[strcspn(line, "\n")] = '\0';
    return strcmp(line, "on") == 0 || strcmp(line, "all") == 0;
}

struct reply {
    int kitty_ok;
    int sixel;
    int da1;
    unsigned px_w, px_h; /* CSI 14 t: the text area in pixels */
};

/* DA1 is "ESC [ ? Pn ; Pn ... c". Returns 1 once a complete one is in buf, and notes whether it lists 4. */
static int parse_da1(const char *buf, size_t len, struct reply *r) {
    for (size_t i = 0; i + 2 < len; i++) {
        if (buf[i] != '\033' || buf[i + 1] != '[' || buf[i + 2] != '?') {
            continue;
        }
        size_t j = i + 3;
        unsigned param = 0;
        int have_digit = 0, sixel = 0;
        for (; j < len; j++) {
            char c = buf[j];
            if (c >= '0' && c <= '9') {
                param = param * 10 + (unsigned) (c - '0');
                have_digit = 1;
            } else if (c == ';' || c == 'c') {
                if (have_digit && param == 4) {
                    sixel = 1;
                }
                param = 0;
                have_digit = 0;
                if (c == 'c') {
                    r->da1 = 1;
                    r->sixel = sixel;
                    return 1;
                }
            } else {
                break; /* not a DA1 reply (another CSI ? sequence) */
            }
        }
    }
    return 0;
}

static void parse_reply(const char *buf, size_t len, struct reply *r) {
    r->kitty_ok = memmem(buf, len, "\033_Gi=31;OK", 10) != NULL;
    for (size_t i = 0; i + 4 < len; i++) {
        unsigned h, w;
        char t;
        if (buf[i] == '\033' && buf[i + 1] == '[' && buf[i + 2] == '4' && buf[i + 3] == ';' &&
            sscanf(buf + i + 4, "%u;%u%c", &h, &w, &t) == 3 && t == 't') {
            r->px_h = h;
            r->px_w = w;
            break;
        }
    }
    parse_da1(buf, len, r);
}

static int timeout_ms(void) {
    const char *v = getenv("RIME_TERM_GRAPHICS_TIMEOUT_MS");
    if (v && *v) {
        char *end;
        long ms = strtol(v, &end, 10);
        if (*end == '\0' && ms >= 1 && ms <= 5000) {
            return (int) ms;
        }
    }
    /* Local terminals answer in well under a millisecond. Over SSH the reply needs a full round trip, and one that
     * misses the deadline is left in the input queue for the shell (see the header), so the remote default
     * covers ordinary internet links, not just a LAN. */
    return is_remote() ? 300 : 35;
}

/* One write, one bounded read. The reply must never reach the shell: echo is off while waiting, and what
 * arrived but was not read (a reply cut off by the deadline) is flushed before the settings come back. What
 * arrives after this process exits cannot be flushed; the deadline (timeout_ms) is what keeps that rare. */
static void query(int ask_pixels, struct reply *r) {
    static const char kitty_q[] = "\033_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\033\\";
    char q[96];
    snprintf(q, sizeof q, "%s%s\033[c", kitty_q, ask_pixels ? "\033[14t" : "");

    if (tcgetattr(tty_fd, &saved_termios) != 0) {
        return;
    }
    struct sigaction sa = { 0 };
    sa.sa_handler = on_signal;
    sigemptyset(&sa.sa_mask);
    int sigs[] = { SIGINT, SIGTERM, SIGHUP, SIGQUIT };
    for (size_t i = 0; i < sizeof sigs / sizeof sigs[0]; i++) {
        sigaction(sigs[i], &sa, NULL);
    }
    signal(SIGTSTP, SIG_IGN);
    termios_saved = 1;

    struct termios raw = saved_termios;
    raw.c_lflag &= (tcflag_t) ~(ICANON | ECHO);
    raw.c_cc[VMIN] = 0;
    raw.c_cc[VTIME] = 0;
    if (tcsetattr(tty_fd, TCSANOW, &raw) != 0) {
        restore_tty();
        return;
    }

    char buf[1024];
    size_t len = 0;
    double deadline = start_ms + timeout_ms();
    if (write(tty_fd, q, strlen(q)) == (ssize_t) strlen(q)) {
        for (;;) {
            double left = deadline - now_ms();
            if (left <= 0) {
                break;
            }
            struct pollfd pfd = { .fd = tty_fd, .events = POLLIN };
            int n = poll(&pfd, 1, (int) left + 1);
            if (n < 0 && errno == EINTR) {
                continue;
            }
            if (n <= 0) {
                break;
            }
            ssize_t got = read(tty_fd, buf + len, sizeof buf - 1 - len);
            if (got <= 0) {
                break;
            }
            len += (size_t) got;
            if (parse_da1(buf, len, r) || len >= sizeof buf - 1) {
                break;
            }
        }
    }
    if (!r->da1) {
        tcflush(tty_fd, TCIFLUSH);
    }
    restore_tty();
    signal(SIGTSTP, SIG_DFL);
    parse_reply(buf, len, r);
}

static enum proto detect(const struct winsize *ws, struct reply *r) {
    const char *term = env("TERM");
    if (!*term || strcmp(term, "dumb") == 0 || strcmp(term, "linux") == 0 || tty_fd < 0) {
        return P_NONE;
    }
    int in_tmux = *env("TMUX") || starts_with(term, "tmux");
    if (!in_tmux && (*env("STY") || starts_with(term, "screen"))) {
        return P_NONE;
    }
    if (in_tmux) {
        if (strcmp(term, "screen") == 0 || !tmux_passthrough()) {
            return P_NONE;
        }
        return (!is_remote() && (is_ghostty() || is_kitty())) ? P_KITTY_DIRECT : P_NONE;
    }
    if (is_alacritty()) {
        return P_NONE;
    }
    if (!is_remote()) {
        if (is_ghostty() || is_kitty()) {
            return P_KITTY_DIRECT;
        }
        if (is_wezterm()) {
            return P_ITERM;
        }
    }
    int have_pixels = ws->ws_xpixel > 0 && ws->ws_ypixel > 0;
    query(!have_pixels, r);
    have_pixels = have_pixels || (r->px_w > 0 && r->px_h > 0);
    if (r->kitty_ok && have_pixels) {
        return P_KITTY;
    }
    if (r->sixel && have_pixels) {
        return P_SIXEL;
    }
    if (says_iterm()) {
        return P_ITERM;
    }
    return P_NONE;
}

/* The PNG's size from its IHDR chunk; 0 when the file is not a PNG. */
static int png_size(const char *path, double *w, double *h) {
    unsigned char b[24];
    FILE *f = fopen(path, "rb");
    if (!f) {
        return 0;
    }
    size_t n = fread(b, 1, sizeof b, f);
    fclose(f);
    if (n != sizeof b || memcmp(b, "\x89PNG\r\n\x1a\n", 8) != 0 || memcmp(b + 12, "IHDR", 4) != 0) {
        return 0;
    }
    unsigned long pw = (unsigned long) b[16] << 24 | (unsigned long) b[17] << 16 | (unsigned long) b[18] << 8 | b[19];
    unsigned long ph = (unsigned long) b[20] << 24 | (unsigned long) b[21] << 16 | (unsigned long) b[22] << 8 | b[23];
    if (pw == 0 || ph == 0) {
        return 0;
    }
    *w = (double) pw;
    *h = (double) ph;
    return 1;
}

static void usage(FILE *to) {
    fputs("usage: rime-term-graphics [--rows N [--image PNG]]\n"
          "Prints the image protocol this terminal can show: kitty-direct, kitty, sixel, iterm or none.\n"
          "With --rows: \"<protocol> <cols> <rows> <cache-token>\", <cols> being the logo's width in cells.\n",
          to);
}

int main(int argc, char **argv) {
    start_ms = now_ms();
    int rows = 0;
    const char *image = NULL;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--rows") == 0 && i + 1 < argc) {
            rows = atoi(argv[++i]);
            if (rows < 1 || rows > 100) {
                usage(stderr);
                return 2;
            }
        } else if (strcmp(argv[i], "--image") == 0 && i + 1 < argc) {
            image = argv[++i];
        } else if (strcmp(argv[i], "--help") == 0 || strcmp(argv[i], "-h") == 0) {
            usage(stdout);
            return 0;
        } else {
            usage(stderr);
            return 2;
        }
    }

    tty_fd = open("/dev/tty", O_RDWR | O_NOCTTY | O_CLOEXEC);
    struct winsize ws = { 0 };
    if (tty_fd < 0 || ioctl(tty_fd, TIOCGWINSZ, &ws) != 0) {
        memset(&ws, 0, sizeof ws);
    }

    char key[512];
    session_key(key, sizeof key);

    struct reply r = { 0 };
    int p = parse_proto(env("RIME_TERM_GRAPHICS_FORCE"));
    if (p < 0) {
        const char *cached = env("RIME_TERM_GRAPHICS");
        const char *at = strchr(cached, '@');
        if (at && strcmp(at + 1, key) == 0) {
            char name[16];
            size_t n = (size_t) (at - cached);
            if (n < sizeof name) {
                memcpy(name, cached, n);
                name[n] = '\0';
                p = parse_proto(name);
            }
        }
    }
    if (p < 0) {
        p = (int) detect(&ws, &r);
    }
    if (tty_fd >= 0) {
        close(tty_fd);
    }

    if (!rows) {
        printf("%s\n", proto_name[p]);
        return 0;
    }

    double ratio = DEFAULT_CELL_RATIO;
    if (ws.ws_xpixel > 0 && ws.ws_ypixel > 0 && ws.ws_col > 0 && ws.ws_row > 0) {
        ratio = ((double) ws.ws_ypixel / ws.ws_row) / ((double) ws.ws_xpixel / ws.ws_col);
    } else if (r.px_w > 0 && r.px_h > 0 && ws.ws_col > 0 && ws.ws_row > 0) {
        ratio = ((double) r.px_h / ws.ws_row) / ((double) r.px_w / ws.ws_col);
    }
    if (ratio < 0.5 || ratio > 4.0) {
        ratio = DEFAULT_CELL_RATIO;
    }
    double iw = DEFAULT_IMG_W, ih = DEFAULT_IMG_H;
    if (image) {
        png_size(image, &iw, &ih);
    }
    int cols = (int) (rows * ratio * iw / ih + 0.5);
    if (cols < 1) {
        cols = 1;
    }
    /* A window too narrow for the logo AND fastfetch's ~45-column info block gets the label; the cache keeps
     * what the terminal can do, so a wider window later still gets the image. */
    int shown = p;
    if (ws.ws_col > 0 && ws.ws_col < cols + 50) {
        shown = P_NONE;
    }
    printf("%s %d %d %s@%s\n", proto_name[shown], cols, rows, proto_name[p], key);
    return 0;
}
