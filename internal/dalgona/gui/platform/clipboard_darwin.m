//go:build darwin && cgo

#import <AppKit/AppKit.h>
#include <stdlib.h>
#include <string.h>
#include "clipboard_darwin.h"

enum {
	DalgonaFormatText = 1,
	DalgonaFormatImage = 2,
	DalgonaFormatFiles = 4,
};

enum {
	DalgonaPasteboardRead = 0,
	DalgonaPasteboardChanged,
	DalgonaPasteboardTooLarge,
	DalgonaPasteboardUnsupported,
	DalgonaPasteboardNotLocal,
	DalgonaPasteboardImageFailed,
	DalgonaPasteboardTooManyPixels,
	DalgonaPasteboardOutOfMemory,
	DalgonaPasteboardWriteFailed,
};

static NSArray<NSPasteboardType> *imageTypes(void) {
	return @[NSPasteboardTypePNG, NSPasteboardTypeTIFF];
}

static int copyOut(NSData *data, size_t limit, dalgona_pasteboard_bytes *out) {
	if (data == nil) {
		return DalgonaPasteboardUnsupported;
	}
	if (data.length > limit) {
		return DalgonaPasteboardTooLarge;
	}
	out->data = malloc(data.length > 0 ? data.length : 1);
	if (out->data == NULL) {
		return DalgonaPasteboardOutOfMemory;
	}
	memcpy(out->data, data.bytes, data.length);
	out->length = data.length;
	return DalgonaPasteboardRead;
}

enum {
	DalgonaTIFFSideLimit = 16384,
};

static int tiffRead(const uint8_t *bytes, size_t length, size_t at, size_t width, int little, uint32_t *value) {
	if ((width != 2 && width != 4) || at > length || length - at < width) {
		return 0;
	}
	if (little) {
		*value = (uint32_t)bytes[at] | ((uint32_t)bytes[at + 1] << 8);
		if (width == 4) {
			*value |= ((uint32_t)bytes[at + 2] << 16) | ((uint32_t)bytes[at + 3] << 24);
		}
		return 1;
	}
	*value = ((uint32_t)bytes[at] << 8) | bytes[at + 1];
	if (width == 4) {
		*value = (*value << 16) | ((uint32_t)bytes[at + 2] << 8) | bytes[at + 3];
	}
	return 1;
}

static int tiffDimensions(NSData *data, uint64_t *wide, uint64_t *high) {
	const uint8_t *bytes = data.bytes;
	size_t length = data.length;
	int little = 0;
	if (length >= 4 && bytes[0] == 'I' && bytes[1] == 'I' && bytes[2] == 42 && bytes[3] == 0) {
		little = 1;
	} else if (length < 4 || bytes[0] != 'M' || bytes[1] != 'M' || bytes[2] != 0 || bytes[3] != 42) {
		return 0;
	}
	uint32_t directory = 0;
	uint32_t count = 0;
	if (!tiffRead(bytes, length, 4, 4, little, &directory) || (size_t)directory + 2 > length || !tiffRead(bytes, length, directory, 2, little, &count)) {
		return 0;
	}
	*wide = 0;
	*high = 0;
	for (uint32_t i = 0; i < count; i++) {
		size_t entry = (size_t)directory + 2 + 12 * (size_t)i;
		uint32_t tag = 0;
		uint32_t kind = 0;
		uint32_t total = 0;
		uint32_t value = 0;
		if (entry > length || length - entry < 12 || !tiffRead(bytes, length, entry, 2, little, &tag) || !tiffRead(bytes, length, entry + 2, 2, little, &kind) || !tiffRead(bytes, length, entry + 4, 4, little, &total) || !tiffRead(bytes, length, entry + 8, kind == 3 ? 2 : 4, little, &value)) {
			return 0;
		}
		if (total != 1 || (kind != 3 && kind != 4)) {
			continue;
		}
		if (tag == 256) {
			*wide = value;
		} else if (tag == 257) {
			*high = value;
		}
	}
	return *wide > 0 && *high > 0;
}

static int readImage(NSPasteboard *pasteboard, size_t limit, uint64_t pixelLimit, dalgona_pasteboard_bytes *out) {
	NSPasteboardType type = [pasteboard availableTypeFromArray:imageTypes()];
	NSData *data = type != nil ? [pasteboard dataForType:type] : nil;
	if (data == nil) {
		return DalgonaPasteboardUnsupported;
	}
	if (data.length > limit) {
		return DalgonaPasteboardTooLarge;
	}
	if ([type isEqualToString:NSPasteboardTypePNG]) {
		return copyOut(data, limit, out);
	}
	uint64_t wide = 0;
	uint64_t high = 0;
	if (tiffDimensions(data, &wide, &high) && (wide > DalgonaTIFFSideLimit || high > DalgonaTIFFSideLimit || high > pixelLimit / wide || 4 * wide * high > (128ULL << 20))) {
		return DalgonaPasteboardTooManyPixels;
	}
	NSBitmapImageRep *image = [NSBitmapImageRep imageRepWithData:data];
	if (image == nil || image.pixelsWide <= 0 || image.pixelsHigh <= 0) {
		return DalgonaPasteboardImageFailed;
	}
	if ((uint64_t)image.pixelsWide > pixelLimit / (uint64_t)image.pixelsHigh) {
		return DalgonaPasteboardTooManyPixels;
	}
	NSData *png = [image representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
	if (png == nil) {
		return DalgonaPasteboardImageFailed;
	}
	return copyOut(png, limit, out);
}

static int readFiles(NSPasteboard *pasteboard, size_t limit, dalgona_pasteboard_bytes *out) {
	NSArray *objects = [pasteboard readObjectsForClasses:@[[NSURL class]] options:@{}];
	if (objects.count == 0) {
		return DalgonaPasteboardUnsupported;
	}
	NSMutableData *list = [NSMutableData data];
	for (NSURL *url in objects) {
		NSURL *located = url.isFileURL ? url.filePathURL : url;
		NSData *line = [[located.absoluteString stringByAppendingString:@"\r\n"] dataUsingEncoding:NSUTF8StringEncoding];
		if (line == nil) {
			return DalgonaPasteboardNotLocal;
		}
		if (line.length > limit - list.length) {
			return DalgonaPasteboardTooLarge;
		}
		[list appendData:line];
	}
	return copyOut(list, limit, out);
}

uint8_t dalgona_pasteboard_formats(long *change) {
	@autoreleasepool {
		NSPasteboard *pasteboard = NSPasteboard.generalPasteboard;
		uint8_t formats = 0;
		*change = (long)pasteboard.changeCount;
		if ([pasteboard availableTypeFromArray:@[NSPasteboardTypeString]] != nil) {
			formats |= DalgonaFormatText;
		}
		if ([pasteboard availableTypeFromArray:imageTypes()] != nil) {
			formats |= DalgonaFormatImage;
		}
		if ([pasteboard canReadObjectForClasses:@[[NSURL class]] options:@{NSPasteboardURLReadingFileURLsOnlyKey: @YES}]) {
			formats |= DalgonaFormatFiles;
		}
		return formats;
	}
}

int dalgona_pasteboard_read(uint8_t format, long change, size_t limit, uint64_t pixel_limit, dalgona_pasteboard_bytes *out) {
	@autoreleasepool {
		NSPasteboard *pasteboard = NSPasteboard.generalPasteboard;
		int status = DalgonaPasteboardUnsupported;
		switch (format) {
		case DalgonaFormatText:
			status = copyOut([pasteboard dataForType:NSPasteboardTypeString], limit, out);
			break;
		case DalgonaFormatImage:
			status = readImage(pasteboard, limit, pixel_limit, out);
			break;
		case DalgonaFormatFiles:
			status = readFiles(pasteboard, limit, out);
			break;
		default:
			break;
		}
		if (status == DalgonaPasteboardRead && (long)pasteboard.changeCount != change) {
			free(out->data);
			out->data = NULL;
			out->length = 0;
			return DalgonaPasteboardChanged;
		}
		return status;
	}
}

int dalgona_pasteboard_write_image(const uint8_t *bytes, size_t length) {
	@autoreleasepool {
		NSData *data = [NSData dataWithBytes:bytes length:length];
		if (data == nil) {
			return DalgonaPasteboardWriteFailed;
		}
		NSPasteboardItem *item = [[NSPasteboardItem alloc] init];
		if (![item setData:data forType:NSPasteboardTypePNG]) {
			return DalgonaPasteboardWriteFailed;
		}
		NSPasteboard *pasteboard = NSPasteboard.generalPasteboard;
		[pasteboard clearContents];
		if (![pasteboard writeObjects:@[item]]) {
			return DalgonaPasteboardWriteFailed;
		}
		return DalgonaPasteboardRead;
	}
}
