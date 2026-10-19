/* System layer: time, exit, messages, directories. src/sdl/i_system.c without
 * SDL, joysticks, the console and the signal handlers; time, sleep, output and
 * the data and home directories go through the Rust seam (lazyos_host.h). */

#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "doomdef.h"
#include "d_clisrv.h"
#include "doomstat.h"
#include "g_game.h"
#include "i_sound.h"
#include "i_system.h"
#include "i_video.h"
#include "m_argv.h"
#include "m_misc.h"
#include "r_fps.h"
#include "w_wad.h"

#include "lazyos_host.h"

UINT8 keyboard_started = 0;

/* ---- time ------------------------------------------------------------ */

precise_t I_GetPreciseTime(void)
{
	return lz_ticks_ns();
}

UINT64 I_GetPrecisePrecision(void)
{
	return 1000000000ULL;
}

static UINT32 frame_rate;
static double frame_frequency;
static UINT64 frame_epoch;
static double elapsed_frames;

static void init_frame_time(const UINT64 now, const UINT32 cap)
{
	frame_rate = cap;
	frame_epoch = now;
	frame_frequency = frame_rate == 0 ? 1.0 : 1000000000.0 / (double)frame_rate;
}

double I_GetFrameTime(void)
{
	const UINT64 now = lz_ticks_ns();
	const UINT32 cap = R_GetFramerateCap();

	if (cap != frame_rate)
		init_frame_time(now, cap);

	if (frame_rate == 0)
		elapsed_frames += 1.0; /* always advance a frame */
	else
		elapsed_frames += (double)(now - frame_epoch) / frame_frequency;

	frame_epoch = now;
	return elapsed_frames;
}

void I_StartupTimer(void)
{
	init_frame_time(0, R_GetFramerateCap());
	elapsed_frames = 0.0;
}

void I_Sleep(UINT32 ms)
{
	lz_sleep_us(ms * 1000U);
}

void I_WaitVBL(INT32 count)
{
	(void)count;
	I_Sleep(1);
}

void I_BeginRead(void) {}
void I_EndRead(void) {}

/* ---- memory and disk ------------------------------------------------- */

/* Only reported to the player (the zone allocator is malloc); a fixed
 * figure is as good as the guest's real one. */
UINT32 I_GetFreeMem(UINT32 *total)
{
	*total = 1024U << 20;
	return 512U << 20;
}

void I_GetDiskFreeSpace(INT64 *freespace)
{
	*freespace = INT32_MAX;
}

/* ---- messages -------------------------------------------------------- */

/* One lz_log call per finished line; a partial line waits for its newline. */
void I_OutputMsg(const char *fmt, ...)
{
	static char line[1024];
	static size_t used;
	char text[8192];
	va_list argptr;
	const char *p;

	va_start(argptr, fmt);
	vsnprintf(text, sizeof text, fmt, argptr);
	va_end(argptr);

	for (p = text; *p; p++)
	{
		if (*p == '\n')
		{
			line[used] = '\0';
			lz_log(line);
			used = 0;
		}
		else if (((unsigned char)*p >= 0x20 && ((unsigned char)*p & 0xF0) != 0x80)
			|| *p == '\t')
		{
			/* the console's colour bytes (0x80..0x8F) are not text */
			if (used < sizeof line - 1)
				line[used++] = *p;
		}
	}
}

INT32 I_GetKey(void)
{
	return 0;
}

/* ---- tic commands ---------------------------------------------------- */

static ticcmd_t emptycmd, emptycmd2, emptycmd3, emptycmd4;

ticcmd_t *I_BaseTiccmd(void)  { return &emptycmd; }
ticcmd_t *I_BaseTiccmd2(void) { return &emptycmd2; }
ticcmd_t *I_BaseTiccmd3(void) { return &emptycmd3; }
ticcmd_t *I_BaseTiccmd4(void) { return &emptycmd4; }

/* ---- startup, exit ---------------------------------------------------- */

static quitfuncptr quit_funcs[MAX_QUIT_FUNCS];

void I_AddExitFunc(void (*func)())
{
	INT32 c;

	for (c = 0; c < MAX_QUIT_FUNCS; c++)
	{
		if (!quit_funcs[c])
		{
			quit_funcs[c] = func;
			break;
		}
	}
}

void I_RemoveExitFunc(void (*func)())
{
	INT32 c;

	for (c = 0; c < MAX_QUIT_FUNCS; c++)
	{
		if (quit_funcs[c] == func)
		{
			for (; c < MAX_QUIT_FUNCS - 1; c++)
				quit_funcs[c] = quit_funcs[c + 1];
			quit_funcs[MAX_QUIT_FUNCS - 1] = NULL;
			break;
		}
	}
}

INT32 I_StartupSystem(void)
{
	return 0;
}

void I_ShutdownSystem(void)
{
	INT32 c;

	for (c = MAX_QUIT_FUNCS - 1; c >= 0; c--)
		if (quit_funcs[c])
			(*quit_funcs[c])();
}

/* Save and stop everything; shared by I_Quit and I_Error. */
static void shut_down(void)
{
	M_SaveConfig(NULL);
	G_SaveGameData(false);

	if (demo.recording)
		G_CheckDemoStatus();
	if (metalrecording)
		G_StopMetalRecording();

	D_QuitNetGame();
	I_ShutdownMusic();
	I_ShutdownSound();
	I_ShutdownCD();
	I_ShutdownGraphics();
	I_ShutdownSystem();
}

void I_Quit(void)
{
	static boolean quitting = false;

	if (!quitting)
	{
		quitting = true;
		shut_down();
		W_Shutdown();
	}
	lz_quit(0);
	exit(0);
}

void I_Error(const char *error, ...)
{
	static boolean shutting_down = false;
	va_list argptr;
	char buffer[8192];

	va_start(argptr, error);
	vsnprintf(buffer, sizeof buffer, error, argptr);
	va_end(argptr);
	I_OutputMsg("\nI_Error(): %s\n", buffer);

	/* an error while shutting down from an error: give up at once */
	if (!shutting_down)
	{
		shutting_down = true;
		shut_down();
		W_Shutdown();
	}
	lz_quit(1);
	exit(-1);
}

/* ---- the machine ------------------------------------------------------ */

char *I_GetUserName(void)
{
	static char username[MAXPLAYERNAME + 1];
	const char *name = getenv("USER");

	if (!name || !*name)
		return NULL;
	strncpy(username, name, MAXPLAYERNAME);
	username[MAXPLAYERNAME] = '\0';
	return username;
}

INT32 I_mkdir(const char *dirname, INT32 unixright)
{
	return mkdir(dirname, (mode_t)unixright);
}

char *I_GetEnv(const char *name)
{
	return getenv(name);
}

INT32 I_PutEnv(char *variable)
{
	return putenv(variable);
}

INT32 I_ClipboardCopy(const char *data, size_t size)
{
	(void)data;
	(void)size;
	return -1;
}

const char *I_ClipboardPaste(void)
{
	return NULL;
}

const CPUInfoFlags *I_CPUInfo(void)
{
	return NULL;
}

void I_RegisterSysCommands(void) {}

/* The directory holding srb2.srb: the data directory the Rust side chose,
 * which becomes the working directory (the engine opens its files by name). */
const char *I_LocateWad(void)
{
	const char *dir = lz_data_dir();
	char probe[1024];
	struct stat st;

	I_OutputMsg("Looking for WADs in: %s\n", dir);
	snprintf(probe, sizeof probe, "%s/srb2.srb", dir);
	if (stat(probe, &st) != 0)
		return NULL;
	if (chdir(dir) == -1)
		I_OutputMsg("Couldn't change working directory\n");
	return dir;
}

/* ---- input devices: none in v1 ---------------------------------------- */

void I_Tactile(FFType t, const JoyFF_t *e)  { (void)t; (void)e; }
void I_Tactile2(FFType t, const JoyFF_t *e) { (void)t; (void)e; }
void I_Tactile3(FFType t, const JoyFF_t *e) { (void)t; (void)e; }
void I_Tactile4(FFType t, const JoyFF_t *e) { (void)t; (void)e; }
void I_JoyScale(void) {}
void I_JoyScale2(void) {}
void I_JoyScale3(void) {}
void I_JoyScale4(void) {}
void I_InitJoystick(void) {}
void I_InitJoystick2(void) {}
void I_InitJoystick3(void) {}
void I_InitJoystick4(void) {}
INT32 I_NumJoys(void) { return 0; }
const char *I_GetJoyName(INT32 joyindex) { (void)joyindex; return NULL; }
void I_GetJoystickEvents(void) {}
void I_GetJoystick2Events(void) {}
void I_GetJoystick3Events(void) {}
void I_GetJoystick4Events(void) {}
void I_GetMouseEvents(void) {}
void I_StartupMouse(void) {}
void I_StartupMouse2(void) {}

#ifndef NOMUMBLE
void I_UpdateMumble(const mobj_t *mobj, const listener_t listener)
{
	(void)mobj;
	(void)listener;
}
#endif
