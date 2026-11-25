//go:build gui && darwin && cgo

#import <AppKit/AppKit.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>
#include <string.h>
#include "dialog_darwin.h"

@interface DalgonaDialogParent : NSObject
@property(weak) NSView *view;
@end

@implementation DalgonaDialogParent
@end

@interface DalgonaDialog : NSObject
- (instancetype)initWithKind:(uint8_t)kind title:(NSString *)title name:(NSString *)name types:(NSArray<UTType *> *)types;
- (void)presentIn:(NSView *)view;
- (void)cancel;
- (int)waitForPaths:(const uint8_t **)paths length:(size_t *)length;
@end

static NSString *readText(const uint8_t *bytes, size_t length) {
	if (length == 0) {
		return @"";
	}
	NSString *text = [[NSString alloc] initWithBytes:bytes length:length encoding:NSUTF8StringEncoding];
	return text != nil ? text : @"";
}

static NSArray<UTType *> *contentTypes(const uint8_t *extensions, size_t length) {
	NSMutableArray<UTType *> *types = [NSMutableArray array];
	size_t start = 0;
	for (size_t i = 0; i <= length; i++) {
		if (i < length && extensions[i] != 0) {
			continue;
		}
		UTType *type = i > start ? [UTType typeWithFilenameExtension:readText(extensions + start, i - start)] : nil;
		if (type != nil) {
			[types addObject:type];
		}
		start = i + 1;
	}
	return types;
}

static NSArray<NSURL *> *chosenURLs(NSSavePanel *panel) {
	if ([panel isKindOfClass:[NSOpenPanel class]]) {
		return ((NSOpenPanel *)panel).URLs;
	}
	return panel.URL != nil ? @[panel.URL] : @[];
}

static NSData *encodePaths(NSArray<NSURL *> *urls) {
	NSMutableData *paths = [NSMutableData data];
	for (NSURL *url in urls) {
		const char *path = url.isFileURL ? url.fileSystemRepresentation : NULL;
		if (path == NULL || path[0] != '/') {
			return nil;
		}
		if (paths.length > 0) {
			[paths appendBytes:"" length:1];
		}
		[paths appendBytes:path length:strlen(path)];
	}
	return paths.length > 0 ? paths : nil;
}

@implementation DalgonaDialog {
	uint8_t _kind;
	NSString *_title;
	NSString *_name;
	NSArray<UTType *> *_types;
	dispatch_semaphore_t _done;
	NSSavePanel *_panel;
	BOOL _finished;
	int _status;
	NSData *_paths;
}

- (instancetype)initWithKind:(uint8_t)kind title:(NSString *)title name:(NSString *)name types:(NSArray<UTType *> *)types {
	self = [super init];
	if (self != nil) {
		_kind = kind;
		_title = title;
		_name = name;
		_types = types;
		_done = dispatch_semaphore_create(0);
	}
	return self;
}

- (void)presentIn:(NSView *)view {
	NSWindow *window = view.window;
	if (window == nil) {
		[self finish:DalgonaDialogNoWindow paths:nil];
		return;
	}
	NSSavePanel *panel = _kind == DalgonaDialogSave ? [self savePanel] : [self openPanel];
	panel.message = _title;
	if (_types.count > 0) {
		panel.allowedContentTypes = _types;
	}
	_panel = panel;
	[panel beginSheetModalForWindow:window completionHandler:^(NSModalResponse response) {
		[self complete:response];
	}];
}

- (NSSavePanel *)savePanel {
	NSSavePanel *panel = [NSSavePanel savePanel];
	panel.nameFieldStringValue = _name;
	panel.canCreateDirectories = YES;
	return panel;
}

- (NSOpenPanel *)openPanel {
	BOOL folder = _kind == DalgonaDialogFolder;
	NSOpenPanel *panel = [NSOpenPanel openPanel];
	panel.canChooseFiles = !folder;
	panel.canChooseDirectories = folder;
	panel.canCreateDirectories = folder;
	panel.allowsMultipleSelection = !folder;
	return panel;
}

- (void)complete:(NSModalResponse)response {
	if (_finished) {
		return;
	}
	if (response != NSModalResponseOK) {
		[self finish:DalgonaDialogCancelled paths:nil];
		return;
	}
	NSData *paths = encodePaths(chosenURLs(_panel));
	[self finish:paths != nil ? DalgonaDialogChosen : DalgonaDialogRemote paths:paths];
}

- (void)cancel {
	NSSavePanel *panel = _panel;
	[panel cancel:nil];
	[self finish:DalgonaDialogCancelled paths:nil];
}

- (void)finish:(int)status paths:(NSData *)paths {
	if (_finished) {
		return;
	}
	_finished = YES;
	_status = status;
	_paths = paths;
	_panel = nil;
	dispatch_semaphore_signal(_done);
}

- (int)waitForPaths:(const uint8_t **)paths length:(size_t *)length {
	dispatch_semaphore_wait(_done, DISPATCH_TIME_FOREVER);
	*paths = _paths.bytes;
	*length = _paths.length;
	return _status;
}

@end

static DalgonaDialog *dialogFrom(uintptr_t dialog) {
	return (__bridge DalgonaDialog *)(void *)dialog;
}

uintptr_t dalgona_dialog_parent(uintptr_t view) {
	@autoreleasepool {
		DalgonaDialogParent *parent = [[DalgonaDialogParent alloc] init];
		parent.view = (__bridge NSView *)(void *)view;
		return (uintptr_t)CFBridgingRetain(parent);
	}
}

void dalgona_dialog_parent_release(uintptr_t parent) {
	CFRelease((CFTypeRef)(void *)parent);
}

uintptr_t dalgona_dialog_begin(uintptr_t parent, uint8_t kind, const uint8_t *title, size_t titleLength, const uint8_t *name, size_t nameLength, const uint8_t *extensions, size_t extensionsLength) {
	@autoreleasepool {
		DalgonaDialogParent *owner = (__bridge DalgonaDialogParent *)(void *)parent;
		DalgonaDialog *dialog = [[DalgonaDialog alloc] initWithKind:kind title:readText(title, titleLength) name:readText(name, nameLength) types:contentTypes(extensions, extensionsLength)];
		dispatch_async(dispatch_get_main_queue(), ^{
			[dialog presentIn:owner.view];
		});
		return (uintptr_t)CFBridgingRetain(dialog);
	}
}

int dalgona_dialog_wait(uintptr_t dialog, const uint8_t **paths, size_t *length) {
	@autoreleasepool {
		return [dialogFrom(dialog) waitForPaths:paths length:length];
	}
}

void dalgona_dialog_cancel(uintptr_t dialog) {
	DalgonaDialog *d = dialogFrom(dialog);
	dispatch_async(dispatch_get_main_queue(), ^{
		[d cancel];
	});
}

void dalgona_dialog_release(uintptr_t dialog) {
	CFRelease((CFTypeRef)(void *)dialog);
}
