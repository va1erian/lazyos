/* Video and input: the software renderer's 8-bit frame goes to Rust with its
 * palette (lz_present), and key events come back (lz_poll_event). Modelled on
 * src/sdl/i_video.c's software path and on src/dummy/i_video.c. */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "doomdef.h"
#include "g_state.h"
#include "command.h"
#include "d_event.h"
#include "d_main.h"
#include "i_system.h"
#include "i_video.h"
#include "m_menu.h"
#include "screen.h"
#include "v_video.h"
#include "z_zone.h"

#include "lazyos_host.h"

rendermode_t rendermode = render_none;
boolean highcolor = false;
boolean allow_fullscreen = false;
UINT8 graphics_started = 0;

static void Impl_SetVsync(void) {}

/* Not used (the host paces frames) but saved in the config by name. */
consvar_t cv_vidwait = {"vid_wait", "Off", CV_SAVE|CV_CALL|CV_NOINIT, CV_OnOff, Impl_SetVsync, 0, NULL, NULL, 0, 0, NULL};

#define MAXWINMODES 6
/* Window sizes the video menu offers: the 16:10 steps of the 320x200 base,
 * then the two 4:3 sizes. */
static const INT32 windowedModes[MAXWINMODES][2] =
{
	{1280, 800},
	{ 960, 600},
	{ 640, 400},
	{ 320, 200},
	{ 640, 480},
	{ 320, 240},
};
static char vidModeName[MAXWINMODES][32];

static UINT8 palette_rgb[256 * 3];
static boolean window_open = false;

/* ---- modes ------------------------------------------------------------ */

INT32 VID_NumModes(void)
{
	return MAXWINMODES;
}

const char *VID_GetModeName(INT32 modeNum)
{
	if (modeNum < 0 || modeNum >= MAXWINMODES)
		return NULL;
	snprintf(vidModeName[modeNum], sizeof vidModeName[modeNum], "%dx%d",
		windowedModes[modeNum][0], windowedModes[modeNum][1]);
	return vidModeName[modeNum];
}

INT32 VID_GetModeForSize(INT32 w, INT32 h)
{
	INT32 i;

	for (i = 0; i < MAXWINMODES; i++)
		if (windowedModes[i][0] == w && windowedModes[i][1] == h)
			return i;
	return -1;
}

void VID_PrepareModeList(void)
{
	allow_fullscreen = false; /* the compositor owns the window */
}

/* The renderer draws into vid.buffer (NUMSCREENS pages of rowbytes*height). */
static void setup_buffer(void)
{
	vid.rowbytes = vid.width * vid.bpp;
	vid.direct = NULL;
	if (vid.buffer)
		free(vid.buffer);
	vid.buffer = calloc(vid.rowbytes * vid.height, NUMSCREENS);
	if (!vid.buffer)
		I_Error("Not enough memory for video buffer\n");
}

INT32 VID_SetMode(INT32 modeNum)
{
	vid.recalc = 1;
	vid.bpp = 1;

	if (modeNum >= 0 && modeNum < MAXWINMODES)
	{
		vid.width = windowedModes[modeNum][0];
		vid.height = windowedModes[modeNum][1];
		vid.modenum = modeNum;
	}
	else
	{
		vid.width = 640;
		vid.height = 400;
		vid.modenum = VID_GetModeForSize(640, 400);
	}

	if (lz_video_open(vid.width, vid.height, "SRB2Kart") != 0)
		I_Error("The window could not be opened (%dx%d)\n", vid.width, vid.height);
	window_open = true;
	setup_buffer();
	return 1;
}

/* ---- start up and shut down -------------------------------------------- */

void I_StartupGraphics(void)
{
	if (dedicated)
	{
		rendermode = render_none;
		return;
	}
	if (graphics_started)
		return;

	CV_RegisterVar(&cv_vidwait);
	keyboard_started = true;
	rendermode = render_soft;
	CONS_Printf("Using the software renderer.\n");

	VID_PrepareModeList();
	VID_SetMode(VID_GetModeForSize(BASEVIDWIDTH, BASEVIDHEIGHT));

	vid.width = BASEVIDWIDTH; /* default size for startup */
	vid.height = BASEVIDHEIGHT;
	vid.recalc = true;
	vid.direct = NULL;
	vid.bpp = 1;
	vid.WndParent = NULL;

	VID_SetMode(VID_GetModeForSize(BASEVIDWIDTH, BASEVIDHEIGHT));

	graphics_started = true;
}

void I_ShutdownGraphics(void)
{
	const rendermode_t old = rendermode;

	rendermode = render_none;
	if (old == render_soft && vid.buffer)
	{
		free(vid.buffer);
		vid.buffer = NULL;
	}
	graphics_started = false;
}

UINT32 I_GetRefreshRate(void)
{
	return 60;
}

/* ---- frames ------------------------------------------------------------ */

void I_SetPalette(RGBA_t *palette)
{
	size_t i;

	for (i = 0; i < 256; i++)
	{
		palette_rgb[i * 3 + 0] = palette[i].s.red;
		palette_rgb[i * 3 + 1] = palette[i].s.green;
		palette_rgb[i * 3 + 2] = palette[i].s.blue;
	}
}

void I_UpdateNoBlit(void) {}

/* One log line per game state change, for session scripts and frame-rate
 * work: "GAMESTATE:<name>". */
static void report_gamestate(void)
{
	static gamestate_t last = (gamestate_t)-1;
	const char *name;
	char line[48];

	if (gamestate == last)
		return;
	last = gamestate;
	switch (gamestate)
	{
	case GS_LEVEL:       name = "level"; break;
	case GS_INTERMISSION: name = "intermission"; break;
	case GS_VOTING:      name = "voting"; break;
	case GS_TITLESCREEN: name = "title"; break;
	case GS_TIMEATTACK:  name = "timeattack"; break;
	case GS_INTRO:       name = "intro"; break;
	case GS_CREDITS:     name = "credits"; break;
	default:             name = "other"; break;
	}
	snprintf(line, sizeof line, "GAMESTATE:%s", name);
	lz_log(line);
}

void I_FinishUpdate(void)
{
	if (rendermode == render_none || !window_open)
		return;

	report_gamestate();

	SCR_CalculateFPS();

	if (cv_ticrate.value)
		SCR_DisplayTicRate();

	if (rendermode == render_soft && screens[0])
		lz_present(screens[0], vid.width, vid.height, (int)vid.rowbytes, palette_rgb);
}

void I_UpdateNoVsync(void)
{
	I_FinishUpdate();
}

void I_ReadScreen(UINT8 *scr)
{
	if (rendermode != render_soft)
		I_Error("I_ReadScreen: called while in non-software mode");
	else
		VID_BlitLinearScreen(screens[0], scr, vid.width * vid.bpp, vid.height,
			vid.rowbytes, vid.rowbytes);
}

/* ---- events ------------------------------------------------------------ */

void I_GetEvent(void)
{
	struct lz_event in;

	while (lz_poll_event(&in))
	{
		event_t event;

		switch (in.kind)
		{
		case 1:
		case 2:
			if (!in.key)
				break;
			memset(&event, 0, sizeof event);
			event.type = in.kind == 1 ? ev_keydown : ev_keyup;
			event.data1 = in.key;
			D_PostEvent(&event);
			break;
		case 3:
			M_QuitResponse('y');
			break;
		default:
			break;
		}
	}
}

void I_OsPolling(void) {}
