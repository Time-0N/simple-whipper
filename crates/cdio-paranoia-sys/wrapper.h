/* Everything bindgen needs to see. cdda.h pulls in <cdio/cdio.h> itself,
 * and paranoia.h depends on the types declared in cdda.h, so order matters. */
#include <cdio/cdio.h>
#include <cdio/paranoia/cdda.h>
#include <cdio/paranoia/paranoia.h>
