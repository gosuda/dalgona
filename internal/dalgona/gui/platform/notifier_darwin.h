#include <stddef.h>
#include <stdint.h>

int dalgona_notifier_bundled(void);
int dalgona_notifier_open(uintptr_t *notifier, int *activations);
uint8_t dalgona_notifier_request(char **failure);
uint8_t dalgona_notifier_settings(char **failure);
char *dalgona_notifier_send(const uint8_t *tag, size_t tag_length, const uint8_t *title, size_t title_length, const uint8_t *body, size_t body_length);
int dalgona_notifier_open_settings(void);
void dalgona_notifier_close(uintptr_t notifier);
