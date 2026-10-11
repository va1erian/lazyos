/* The seam between SRB2Kart's platform layer (this directory, C) and the Rust
 * program that owns `main` (srb2kart/src/hooks.rs). docs/srb2kart-port-plan.md,
 * "The C <-> Rust seam". Everything Messenger-shaped stays on the Rust side. */
#ifndef LAZYOS_HOST_H
#define LAZYOS_HOST_H

#include <stdint.h>

struct lz_event {
	int32_t kind; /* 1 key down, 2 key up, 3 close */
	int32_t key;  /* SRB2 keys.h code (KEY_*, ASCII) */
};

/* Rust -> C: called once by Rust main; never returns (I_Quit exits). */
int srb2kart_main(int argc, char **argv);

/* C -> Rust */
void        lz_log(const char *line);              /* I_OutputMsg, one line */
uint64_t    lz_ticks_ns(void);                     /* monotonic since start */
void        lz_sleep_us(uint32_t us);
int         lz_video_open(int w, int h, const char *title); /* 0 ok */
void        lz_present(const uint8_t *pixels, int w, int h, int pitch,
                       const uint8_t palette_rgb[256 * 3]); /* 8-bit frame */
int         lz_poll_event(struct lz_event *out);   /* 1 when *out was filled */
const char *lz_data_dir(void);                     /* /system/share/srb2kart */
const char *lz_home_dir(void);                     /* the writable config dir, created */
void        lz_quit(int status);                   /* QUIT marker, exit */

#endif
