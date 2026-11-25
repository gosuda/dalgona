#include <stdbool.h>
#include <stdint.h>

bool dalgona_preferences_reduce_motion(void);
bool dalgona_preferences_increase_contrast(void);
bool dalgona_preferences_dark(void);
int dalgona_appearance_watch(uintptr_t events, void **token);
void dalgona_appearance_unwatch(void *token);
