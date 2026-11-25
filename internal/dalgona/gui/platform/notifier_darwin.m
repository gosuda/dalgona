//go:build darwin && cgo

#import <AppKit/AppKit.h>
#import <UserNotifications/UserNotifications.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "notifier_darwin.h"

enum {
	DalgonaAuthorizationNotDetermined = 0,
	DalgonaAuthorizationDenied = 1,
	DalgonaAuthorizationAuthorized = 2,
	DalgonaAuthorizationProvisional = 3,
	DalgonaAuthorizationUnknown = 4,
};

enum {
	DalgonaTagLimit = 4096,
	DalgonaActivationLimit = 16,
};

static void writeActivation(int fd, const uint8_t *p, size_t length) {
	while (length > 0) {
		ssize_t written = write(fd, p, length);
		if (written < 0 && errno == EINTR) {
			continue;
		}
		if (written <= 0) {
			return;
		}
		p += written;
		length -= (size_t)written;
	}
}

static NSString *nativeText(const uint8_t *bytes, size_t length) {
	if (length == 0) {
		return @"";
	}
	return [[NSString alloc] initWithBytes:bytes length:length encoding:NSUTF8StringEncoding];
}

static char *failureText(NSError *error) {
	if (error == nil) {
		return NULL;
	}
	const char *description = error.localizedDescription.UTF8String;
	return strdup(description != NULL ? description : "UserNotifications reported an error without a description");
}

@interface DalgonaNotificationDelegate : NSObject <UNUserNotificationCenterDelegate>
- (instancetype)initWithActivations:(int)activations;
- (void)close;
@end

@implementation DalgonaNotificationDelegate {
	int _activations;
	dispatch_queue_t _writer;
	BOOL _closed;
	NSUInteger _inflight;
}

- (instancetype)initWithActivations:(int)activations {
	self = [super init];
	if (self != nil) {
		_activations = activations;
		_writer = dispatch_queue_create("com.gosuda.dalgona.notification.activations", DISPATCH_QUEUE_SERIAL);
	}
	return self;
}

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
	didReceiveNotificationResponse:(UNNotificationResponse *)response
		 withCompletionHandler:(void (^)(void))completionHandler {
	if ([response.actionIdentifier isEqualToString:UNNotificationDefaultActionIdentifier]) {
		[self forward:response.notification.request.identifier];
	}
	completionHandler();
}

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
	   willPresentNotification:(UNNotification *)notification
		 withCompletionHandler:(void (^)(UNNotificationPresentationOptions))completionHandler {
	completionHandler(UNNotificationPresentationOptionBanner | UNNotificationPresentationOptionList);
}

- (void)forward:(NSString *)tag {
	NSData *bytes = [tag dataUsingEncoding:NSUTF8StringEncoding];
	if (bytes.length == 0 || bytes.length > DalgonaTagLimit) {
		return;
	}
	NSMutableData *record = [NSMutableData dataWithLength:4 + bytes.length];
	uint8_t *p = record.mutableBytes;
	uint32_t length = (uint32_t)bytes.length;
	p[0] = (uint8_t)length;
	p[1] = (uint8_t)(length >> 8);
	p[2] = (uint8_t)(length >> 16);
	p[3] = (uint8_t)(length >> 24);
	memcpy(p + 4, bytes.bytes, bytes.length);
	@synchronized(self) {
		if (_closed || _inflight >= DalgonaActivationLimit) {
			return;
		}
		_inflight++;
		int activations = _activations;
		dispatch_async(_writer, ^{
			writeActivation(activations, record.bytes, record.length);
			@synchronized(self) {
				self->_inflight--;
			}
		});
	}
}

- (void)close {
	@synchronized(self) {
		if (_closed) {
			return;
		}
		_closed = YES;
		int activations = _activations;
		dispatch_async(_writer, ^{
			close(activations);
		});
	}
	UNUserNotificationCenter *center = UNUserNotificationCenter.currentNotificationCenter;
	if (center.delegate == self) {
		center.delegate = nil;
	}
}

@end

static uint8_t authorizationFor(UNAuthorizationStatus status) {
	switch (status) {
	case UNAuthorizationStatusNotDetermined:
		return DalgonaAuthorizationNotDetermined;
	case UNAuthorizationStatusDenied:
		return DalgonaAuthorizationDenied;
	case UNAuthorizationStatusAuthorized:
		return DalgonaAuthorizationAuthorized;
	case UNAuthorizationStatusProvisional:
		return DalgonaAuthorizationProvisional;
	default:
		return DalgonaAuthorizationUnknown;
	}
}

static uint8_t currentAuthorization(UNUserNotificationCenter *center) {
	dispatch_semaphore_t done = dispatch_semaphore_create(0);
	__block UNAuthorizationStatus status = UNAuthorizationStatusNotDetermined;
	__block BOOL answered = NO;
	[center getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings *settings) {
		status = settings.authorizationStatus;
		answered = settings != nil;
		dispatch_semaphore_signal(done);
	}];
	dispatch_semaphore_wait(done, DISPATCH_TIME_FOREVER);
	return answered ? authorizationFor(status) : DalgonaAuthorizationUnknown;
}

int dalgona_notifier_bundled(void) {
	@autoreleasepool {
		return NSBundle.mainBundle.bundleIdentifier != nil;
	}
}

int dalgona_notifier_open(uintptr_t *notifier, int *activations) {
	@autoreleasepool {
		int fds[2];
		if (pipe(fds) != 0) {
			return errno;
		}
		if (fcntl(fds[0], F_SETFD, FD_CLOEXEC) != 0 || fcntl(fds[1], F_SETFD, FD_CLOEXEC) != 0 || fcntl(fds[1], F_SETNOSIGPIPE, 1) != 0) {
			int code = errno;
			close(fds[0]);
			close(fds[1]);
			return code;
		}
		DalgonaNotificationDelegate *delegate = [[DalgonaNotificationDelegate alloc] initWithActivations:fds[1]];
		UNUserNotificationCenter.currentNotificationCenter.delegate = delegate;
		*notifier = (uintptr_t)CFBridgingRetain(delegate);
		*activations = fds[0];
		return 0;
	}
}

uint8_t dalgona_notifier_request(char **failure) {
	@autoreleasepool {
		UNUserNotificationCenter *center = UNUserNotificationCenter.currentNotificationCenter;
		dispatch_semaphore_t done = dispatch_semaphore_create(0);
		__block NSError *problem = nil;
		[center requestAuthorizationWithOptions:UNAuthorizationOptionAlert
							  completionHandler:^(BOOL granted, NSError *error) {
								  problem = error;
								  dispatch_semaphore_signal(done);
							  }];
		dispatch_semaphore_wait(done, DISPATCH_TIME_FOREVER);
		*failure = failureText(problem);
		return currentAuthorization(center);
	}
}

uint8_t dalgona_notifier_settings(char **failure) {
	@autoreleasepool {
		*failure = NULL;
		return currentAuthorization(UNUserNotificationCenter.currentNotificationCenter);
	}
}

char *dalgona_notifier_send(const uint8_t *tag, size_t tag_length, const uint8_t *title, size_t title_length, const uint8_t *body, size_t body_length) {
	@autoreleasepool {
		NSString *identifier = nativeText(tag, tag_length);
		NSString *heading = nativeText(title, title_length);
		NSString *message = nativeText(body, body_length);
		if (identifier == nil || heading == nil || message == nil) {
			return strdup("the notification text is not valid UTF-8");
		}
		UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
		content.title = heading;
		content.body = message;
		UNNotificationRequest *request = [UNNotificationRequest requestWithIdentifier:identifier content:content trigger:nil];
		dispatch_semaphore_t done = dispatch_semaphore_create(0);
		__block NSError *problem = nil;
		[UNUserNotificationCenter.currentNotificationCenter addNotificationRequest:request
															 withCompletionHandler:^(NSError *error) {
																 problem = error;
																 dispatch_semaphore_signal(done);
															 }];
		dispatch_semaphore_wait(done, DISPATCH_TIME_FOREVER);
		return failureText(problem);
	}
}

int dalgona_notifier_open_settings(void) {
	@autoreleasepool {
		NSString *pane = @"x-apple.systempreferences:com.apple.Notifications-Settings.extension";
		NSString *bundle = NSBundle.mainBundle.bundleIdentifier;
		NSURL *app = [NSURL URLWithString:bundle != nil ? [pane stringByAppendingFormat:@"?id=%@", bundle] : pane];
		if (app != nil && [NSWorkspace.sharedWorkspace openURL:app]) {
			return 1;
		}
		NSURL *legacy = [NSURL URLWithString:@"x-apple.systempreferences:com.apple.preference.notifications"];
		return legacy != nil && [NSWorkspace.sharedWorkspace openURL:legacy] ? 1 : 0;
	}
}

void dalgona_notifier_close(uintptr_t notifier) {
	@autoreleasepool {
		DalgonaNotificationDelegate *delegate = CFBridgingRelease((CFTypeRef)(void *)notifier);
		[delegate close];
	}
}
