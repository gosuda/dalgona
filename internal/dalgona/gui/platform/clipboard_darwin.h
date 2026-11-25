#include <stddef.h>
#include <stdint.h>

typedef struct {
	uint8_t *data;
	size_t length;
} dalgona_pasteboard_bytes;

uint8_t dalgona_pasteboard_formats(long *change);
int dalgona_pasteboard_read(uint8_t format, long change, size_t limit, uint64_t pixel_limit, dalgona_pasteboard_bytes *out);
int dalgona_pasteboard_write_image(const uint8_t *bytes, size_t length);
