//go:build darwin && cgo

#import <AppKit/AppKit.h>
#include "preferences_darwin.h"
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>

@interface DalgonaAppearanceObserver : NSObject
@property(nonatomic) int events;
- (void)closeEvents;
@end

@implementation DalgonaAppearanceObserver
- (void)notify {
	@synchronized (self) {
		if (self.events >= 0) {
			uint8_t changed = 1;
			(void)write(self.events, &changed, sizeof changed);
		}
	}
}

- (void)closeEvents {
	@synchronized (self) {
		self.events = -1;
	}
}

- (void)observeValueForKeyPath:(NSString *)keyPath ofObject:(id)object change:(NSDictionary *)change context:(void *)context {
	[self notify];
}

- (void)displayOptionsChanged:(NSNotification *)note {
	[self notify];
}
@end

bool dalgona_preferences_reduce_motion(void) {
	@autoreleasepool {
		return NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion;
	}
}

bool dalgona_preferences_increase_contrast(void) {
	@autoreleasepool {
		return NSWorkspace.sharedWorkspace.accessibilityDisplayShouldIncreaseContrast;
	}
}

bool dalgona_preferences_dark(void) {
	CFPreferencesAppSynchronize(kCFPreferencesAnyApplication);
	CFPropertyListRef style = CFPreferencesCopyAppValue(CFSTR("AppleInterfaceStyle"), kCFPreferencesAnyApplication);
	if (style == NULL) {
		return false;
	}
	bool dark = CFGetTypeID(style) == CFStringGetTypeID() && CFStringCompare((CFStringRef)style, CFSTR("Dark"), 0) == kCFCompareEqualTo;
	CFRelease(style);
	return dark;
}

int dalgona_appearance_watch(uintptr_t events, void **token) {
	int fd = (int)events;
	int flags = fcntl(fd, F_GETFL);
	if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) < 0 || fcntl(fd, F_SETNOSIGPIPE, 1) < 0) {
		return errno;
	}
	DalgonaAppearanceObserver *observer = [DalgonaAppearanceObserver new];
	observer.events = fd;
	dispatch_async(dispatch_get_main_queue(), ^{
		[NSApplication.sharedApplication addObserver:observer forKeyPath:@"effectiveAppearance" options:0 context:NULL];
		[NSWorkspace.sharedWorkspace.notificationCenter addObserver:observer selector:@selector(displayOptionsChanged:) name:NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification object:nil];
	});
	*token = (__bridge_retained void *)observer;
	return 0;
}

void dalgona_appearance_unwatch(void *token) {
	DalgonaAppearanceObserver *observer = (__bridge_transfer DalgonaAppearanceObserver *)token;
	[observer closeEvents];
	dispatch_sync(dispatch_get_main_queue(), ^{
		[NSApplication.sharedApplication removeObserver:observer forKeyPath:@"effectiveAppearance"];
		[NSWorkspace.sharedWorkspace.notificationCenter removeObserver:observer];
	});
}
