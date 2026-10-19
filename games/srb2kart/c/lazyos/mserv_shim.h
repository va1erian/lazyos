/* Force-included into mserv.c only (build.rs). Without libcurl the master
 * server code (MASTERSERVER) is compiled out, but Advertise_OnChange outside
 * it still uses these; upstream only ever builds the file with curl. */
#define Lock_state()
#define Unlock_state()
#define MSId 0
#define MSRegisteredId 0
