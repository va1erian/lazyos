/* Force-included into d_clisrv.c only (build.rs). Its "downloading" screen
 * names CL_DOWNLOADHTTPFILES outside the HAVE_CURL block that declares it;
 * upstream only ever builds the file with curl. No mode has this value, so the
 * comparison is false. */
#define CL_DOWNLOADHTTPFILES (-1)
