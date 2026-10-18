/* Things upstream gets from its SDL build and its libcurl build, which this
 * platform layer has neither of:
 *
 * - the joystick console variables d_netcmd.c defines only under HAVE_SDL
 *   (registered by D_RegisterClientCommands, so they must exist; no joystick
 *   is ever found, see I_NumJoys);
 * - strupr and strlwr (src/sdl/dosstr.c);
 * - the master server entry points (mserv.c, MASTERSERVER only), which
 *   d_clisrv.c calls unconditionally: there is no master server. */

#include "doomdef.h"
#include "command.h"
#include "d_netcmd.h"
#include "i_system.h"
#include "mserv.h"

#include "sdl/dosstr.c"

consvar_t cv_usejoystick = {"use_joystick", "1", CV_SAVE|CV_CALL, NULL, I_InitJoystick, 0, NULL, NULL, 0, 0, NULL};
consvar_t cv_usejoystick2 = {"use_joystick2", "2", CV_SAVE|CV_CALL, NULL, I_InitJoystick2, 0, NULL, NULL, 0, 0, NULL};
consvar_t cv_usejoystick3 = {"use_joystick3", "3", CV_SAVE|CV_CALL, NULL, I_InitJoystick3, 0, NULL, NULL, 0, 0, NULL};
consvar_t cv_usejoystick4 = {"use_joystick4", "4", CV_SAVE|CV_CALL, NULL, I_InitJoystick4, 0, NULL, NULL, 0, 0, NULL};

consvar_t cv_joyscale3 = {"joyscale3", "1", CV_SAVE|CV_CALL, NULL, I_JoyScale3, 0, NULL, NULL, 0, 0, NULL};
consvar_t cv_joyscale4 = {"joyscale4", "1", CV_SAVE|CV_CALL, NULL, I_JoyScale4, 0, NULL, NULL, 0, 0, NULL};

void UnregisterServer(void) {}
void MasterClient_Ticker(void) {}
