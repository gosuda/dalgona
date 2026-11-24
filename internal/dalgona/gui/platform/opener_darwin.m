//go:build darwin && cgo

#import <AppKit/AppKit.h>
#include "opener_darwin.h"

enum {
	DalgonaOpened = 0,
	DalgonaOpenRefused = 1,
	DalgonaOpenUnreadable = 2,
};

static NSString *readText(const uint8_t *bytes, size_t length) {
	if (length == 0) {
		return nil;
	}
	return [[NSString alloc] initWithBytes:bytes length:length encoding:NSUTF8StringEncoding];
}

static int openTarget(NSURL *target) {
	if (target == nil) {
		return DalgonaOpenUnreadable;
	}
	return [NSWorkspace.sharedWorkspace openURL:target] ? DalgonaOpened : DalgonaOpenRefused;
}

int dalgona_open_url(const uint8_t *url, size_t length) {
	@autoreleasepool {
		NSString *text = readText(url, length);
		return openTarget(text != nil ? [NSURL URLWithString:text] : nil);
	}
}

int dalgona_open_path(const uint8_t *path, size_t length) {
	@autoreleasepool {
		NSString *text = readText(path, length);
		return openTarget(text != nil ? [NSURL fileURLWithPath:text] : nil);
	}
}

int dalgona_reveal_path(const uint8_t *path, size_t length) {
	@autoreleasepool {
		NSString *text = readText(path, length);
		if (text == nil) {
			return DalgonaOpenUnreadable;
		}
		[NSWorkspace.sharedWorkspace activateFileViewerSelectingURLs:@[[NSURL fileURLWithPath:text]]];
		return DalgonaOpened;
	}
}
