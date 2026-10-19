/* Entry point. Rust owns `main` and calls srb2kart_main (lazyos_host.h); the
 * body is src/sdl/i_main.c without SDL, TTF, Win32 and the log file (the
 * output goes through lz_log instead). */

#include <stdlib.h>

#include "doomdef.h"
#include "m_argv.h"
#include "d_main.h"
#include "i_system.h"
#include "lazyos_host.h"

/* The engine keeps its config under `<-home>/<DEFAULTDIR>` (D_Home); Rust
 * chose the directory, so hand it over as `-home`. */
int srb2kart_main(int argc, char **argv)
{
	char **args = malloc((size_t)(argc + 3) * sizeof *args);
	int i;

	if (!args)
		return 1;
	for (i = 0; i < argc; i++)
		args[i] = argv[i];
	args[argc++] = (char *)"-home";
	args[argc++] = (char *)lz_home_dir();
	args[argc] = NULL;

	myargc = argc;
	myargv = args; /* kept for the whole run (M_CheckParm) */

	I_StartupSystem();

	CONS_Printf("Setting up SRB2Kart...\n");
	D_SRB2Main();
	CONS_Printf("Entering main game loop...\n");
	/* never returns: I_Quit and I_Error end in lz_quit */
	D_SRB2Loop();
	return 0;
}
