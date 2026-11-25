#include <stddef.h>
#include <stdint.h>

int dalgona_ax_attach(uintptr_t view, uintptr_t *bridge, int *actions);
void dalgona_ax_publish(uintptr_t bridge, const uint8_t *snapshot, size_t length);
void dalgona_ax_announce(uintptr_t bridge, const uint8_t *text, size_t length, uint8_t priority);
void dalgona_ax_detach(uintptr_t bridge);
