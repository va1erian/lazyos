/* Network driver: none in v1 (NONET; K3 brings direct-IP netplay). With NONET
 * i_tcp.c still defines the driver function pointers; this says there is no
 * network to start. */

#include "doomdef.h"
#include "i_net.h"

boolean I_InitNetwork(void)
{
	return false;
}
