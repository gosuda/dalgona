//go:build darwin && cgo

#import <AppKit/AppKit.h>
#import <objc/runtime.h>
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <string.h>
#include <unistd.h>
#include "accessibility_darwin.h"

static const uint32_t DalgonaNone = 0xFFFFFFFFu;

enum {
	DalgonaSnapshotHeader = 16,
	DalgonaNodeHeader = 96,
	DalgonaBoxHeader = 24,
	DalgonaClusterSize = 12,
	DalgonaActionHeader = 28,
	DalgonaActionLimit = 64,
	DalgonaValueUnitLimit = 16 << 20,
	DalgonaTextObjectLimit = 64 << 10,
};

enum {
	DalgonaRoleGroup = 1,
	DalgonaRoleButton,
	DalgonaRoleCheckBox,
	DalgonaRoleRadioButton,
	DalgonaRoleTextField,
	DalgonaRoleTextArea,
	DalgonaRoleStaticText,
	DalgonaRoleLink,
	DalgonaRoleMenuItem,
	DalgonaRoleMenu,
	DalgonaRoleHeading,
	DalgonaRoleList,
	DalgonaRoleRow,
	DalgonaRoleTable,
	DalgonaRoleCell,
	DalgonaRoleImage,
};

enum {
	DalgonaSubroleNone = 0,
	DalgonaSubroleSwitch,
	DalgonaSubroleSecureTextField,
	DalgonaSubroleContentList,
	DalgonaSubroleTableRow,
	DalgonaSubroleDialog,
};

enum {
	DalgonaDisabled = 1 << 0,
	DalgonaChecked = 1 << 1,
	DalgonaPressable = 1 << 2,
	DalgonaEditable = 1 << 3,
	DalgonaText = 1 << 4,
	DalgonaCheckable = 1 << 5,
	DalgonaColumnHeader = 1 << 6,
	DalgonaRowHeader = 1 << 7,
};

enum {
	DalgonaSelectable = 1 << 0,
	DalgonaSelected = 1 << 1,
	DalgonaExpandable = 1 << 2,
	DalgonaExpanded = 1 << 3,
	DalgonaTextUncached = 1 << 4,
	DalgonaTextLoading = 1 << 5,
	DalgonaTextFailed = 1 << 6,
	DalgonaTextContainer = 1 << 7,
	DalgonaTextSelectable = 1 << 8,
};

enum {
	DalgonaActionActivate = 1,
	DalgonaActionFocus = 2,
	DalgonaActionSetValue = 3,
	DalgonaActionSelectText = 4,
	DalgonaActionLoadText = 6,
};

enum {
	DalgonaPriorityLow = 1,
	DalgonaPriorityMedium = 2,
	DalgonaPriorityHigh = 3,
};

typedef struct {
	uint32_t parent;
	uint32_t target;
	uint8_t role;
	uint8_t subrole;
	uint8_t abilities;
	uint8_t level;
	NSRect pixels;
	uint32_t location;
	uint32_t length;
	const uint8_t *identity;
	uint32_t identityLength;
	const uint8_t *texts[5];
	uint32_t units[5];
	uint32_t states;
	uint32_t textGroup;
	uint32_t textIndex;
	uint32_t row;
	uint32_t rows;
	uint32_t column;
	uint32_t columns;
	const uint8_t *lines;
	uint32_t lineCount;
	const uint8_t *boxes;
	uint32_t boxCount;
	uint64_t boxBytes;
} DalgonaNode;

typedef struct {
	uint32_t line;
	NSRect pixels;
	uint32_t count;
	const uint8_t *clusters;
} DalgonaBox;

static uint32_t readU32(const uint8_t *p) {
	return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static uint64_t readU64(const uint8_t *p) {
	return (uint64_t)readU32(p) | (uint64_t)readU32(p + 4) << 32;
}

static int32_t readI32(const uint8_t *p) {
	return (int32_t)readU32(p);
}

static NSRect readPixels(const uint8_t *p) {
	int32_t x0 = readI32(p);
	int32_t y0 = readI32(p + 4);
	int32_t x1 = readI32(p + 8);
	int32_t y1 = readI32(p + 12);
	return NSMakeRect(x0, y0, (CGFloat)x1 - x0, (CGFloat)y1 - y0);
}

static const uint8_t *readBox(const uint8_t *p, DalgonaBox *box) {
	box->line = readU32(p);
	box->pixels = readPixels(p + 4);
	box->count = readU32(p + 20);
	box->clusters = p + DalgonaBoxHeader;
	return box->clusters + (size_t)box->count * DalgonaClusterSize;
}

static uint32_t clusterStart(const DalgonaBox *box, uint32_t k) {
	return readU32(box->clusters + (size_t)k * DalgonaClusterSize);
}

static int32_t clusterLeft(const DalgonaBox *box, uint32_t k) {
	return readI32(box->clusters + (size_t)k * DalgonaClusterSize + 4);
}

static int32_t clusterRight(const DalgonaBox *box, uint32_t k) {
	return readI32(box->clusters + (size_t)k * DalgonaClusterSize + 8);
}

static NSRect clusterPixels(const DalgonaBox *box, CGFloat left, CGFloat right) {
	return NSMakeRect(left, NSMinY(box->pixels), right - left, NSHeight(box->pixels));
}

static NSRect coverPixels(NSRect a, NSRect b) {
	CGFloat x0 = MIN(NSMinX(a), NSMinX(b));
	CGFloat y0 = MIN(NSMinY(a), NSMinY(b));
	CGFloat x1 = MAX(NSMaxX(a), NSMaxX(b));
	CGFloat y1 = MAX(NSMaxY(a), NSMaxY(b));
	return NSMakeRect(x0, y0, x1 - x0, y1 - y0);
}

static void writeU32(uint8_t *p, uint32_t v) {
	p[0] = (uint8_t)v;
	p[1] = (uint8_t)(v >> 8);
	p[2] = (uint8_t)(v >> 16);
	p[3] = (uint8_t)(v >> 24);
}

static void writeU64(uint8_t *p, uint64_t v) {
	writeU32(p, (uint32_t)v);
	writeU32(p + 4, (uint32_t)(v >> 32));
}

static NSString *readText(const uint8_t *p, uint32_t units) {
	NSString *text = [[NSString alloc] initWithBytes:p length:(NSUInteger)units * 2 encoding:NSUTF16LittleEndianStringEncoding];
	return text != nil ? text : @"";
}

static void writeAll(int fd, const uint8_t *p, size_t length) {
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

static NSAccessibilityRole headingRole(void) {
#ifdef __MAC_26_0
	if (@available(macOS 26.0, *)) {
		return NSAccessibilityHeadingRole;
	}
#endif
	return NSAccessibilityGroupRole;
}

static NSAccessibilityRole roleFor(uint8_t role) {
	switch (role) {
	case DalgonaRoleButton:
		return NSAccessibilityButtonRole;
	case DalgonaRoleCheckBox:
		return NSAccessibilityCheckBoxRole;
	case DalgonaRoleRadioButton:
		return NSAccessibilityRadioButtonRole;
	case DalgonaRoleTextField:
		return NSAccessibilityTextFieldRole;
	case DalgonaRoleTextArea:
		return NSAccessibilityTextAreaRole;
	case DalgonaRoleStaticText:
		return NSAccessibilityStaticTextRole;
	case DalgonaRoleLink:
		return NSAccessibilityLinkRole;
	case DalgonaRoleMenuItem:
		return NSAccessibilityMenuItemRole;
	case DalgonaRoleMenu:
		return NSAccessibilityMenuRole;
	case DalgonaRoleHeading:
		return headingRole();
	case DalgonaRoleList:
		return NSAccessibilityListRole;
	case DalgonaRoleRow:
		return NSAccessibilityRowRole;
	case DalgonaRoleTable:
		return NSAccessibilityTableRole;
	case DalgonaRoleCell:
		return NSAccessibilityCellRole;
	case DalgonaRoleImage:
		return NSAccessibilityImageRole;
	default:
		return NSAccessibilityGroupRole;
	}
}

static NSAccessibilitySubrole subroleFor(uint8_t subrole) {
	switch (subrole) {
	case DalgonaSubroleSwitch:
		return NSAccessibilitySwitchSubrole;
	case DalgonaSubroleSecureTextField:
		return NSAccessibilitySecureTextFieldSubrole;
	case DalgonaSubroleContentList:
		return NSAccessibilityContentListSubrole;
	case DalgonaSubroleTableRow:
		return NSAccessibilityTableRowSubrole;
	case DalgonaSubroleDialog:
		return NSAccessibilityDialogSubrole;
	default:
		return nil;
	}
}

static NSAccessibilityPriorityLevel priorityFor(uint8_t priority) {
	switch (priority) {
	case DalgonaPriorityLow:
		return NSAccessibilityPriorityLow;
	case DalgonaPriorityHigh:
		return NSAccessibilityPriorityHigh;
	default:
		return NSAccessibilityPriorityMedium;
	}
}

@class DalgonaAccessibilityBridge;

@interface DalgonaAccessibilityElement : NSAccessibilityElement
@property (weak) DalgonaAccessibilityBridge *nodeBridge;
@property (copy) NSData *nodeIdentity;
@property BOOL nodeLive;
@property uint64_t nodeRevision;
@property uint8_t nodeRole;
@property uint8_t nodeSubrole;
@property uint8_t nodeAbilities;
@property uint8_t nodeLevel;
@property NSRect nodePixels;
@property (copy) NSString *nodeLabel;
@property (copy) NSString *nodeHelp;
@property (copy) NSString *nodeText;
@property (copy) NSString *nodePlaceholder;
@property uint32_t nodeStates;
@property NSRange nodeSelection;
@property (weak) id nodeParent;
@property (copy) NSString *nodeTextBefore;
@property uint32_t nodeTextIndex;
@property (copy) NSArray<DalgonaAccessibilityElement *> *nodeTextParts;
@property (copy) NSArray *nodeChildren;
@property (copy) NSArray *nodeLinked;
@property (copy) NSArray *nodeRows;
@property (copy) NSArray *nodeHeaders;
@property (copy) NSArray *nodeRowHeaders;
@property NSInteger nodeIndex;
@property NSInteger nodeColumnCount;
@property NSRange nodeRowRange;
@property NSRange nodeColumnRange;
@property (copy) NSData *nodeLines;
@property (copy) NSData *nodeBoxes;
- (BOOL)has:(uint8_t)ability;
@end

@interface DalgonaAccessibilityBridge : NSObject
@property (weak) NSView *view;
- (instancetype)initWithView:(NSView *)view actions:(int)actions;
- (void)install;
- (void)publish:(NSData *)snapshot;
- (void)announce:(NSString *)text priority:(NSAccessibilityPriorityLevel)priority;
- (void)detach;
- (NSArray *)rootChildren;
- (id)focusedElement;
- (id)hitTest:(NSPoint)point;
- (NSRect)screenFrame:(NSRect)pixels;
- (BOOL)pixelPoint:(NSPoint)point into:(NSPoint *)pixel;
- (BOOL)send:(uint8_t)action element:(DalgonaAccessibilityElement *)element range:(NSRange)range value:(NSString *)value;
@end

@interface DalgonaAccessibilityGroup : DalgonaAccessibilityElement <NSAccessibilityGroup>
@end
@implementation DalgonaAccessibilityGroup
@end

@interface DalgonaAccessibilityButton : DalgonaAccessibilityElement <NSAccessibilityButton>
@end
@implementation DalgonaAccessibilityButton
@end

@interface DalgonaAccessibilityCheckBox : DalgonaAccessibilityElement <NSAccessibilityCheckBox>
@end
@implementation DalgonaAccessibilityCheckBox
@end

@interface DalgonaAccessibilityRadioButton : DalgonaAccessibilityElement <NSAccessibilityRadioButton>
@end
@implementation DalgonaAccessibilityRadioButton
@end

@interface DalgonaAccessibilityText : DalgonaAccessibilityElement <NSAccessibilityNavigableStaticText>
@end
@implementation DalgonaAccessibilityText
@end

@interface DalgonaAccessibilityImage : DalgonaAccessibilityElement <NSAccessibilityImage>
@end
@implementation DalgonaAccessibilityImage
@end

@interface DalgonaAccessibilityList : DalgonaAccessibilityElement <NSAccessibilityList>
@end
@implementation DalgonaAccessibilityList
@end

@interface DalgonaAccessibilityRow : DalgonaAccessibilityElement <NSAccessibilityRow>
@end
@implementation DalgonaAccessibilityRow
@end

@interface DalgonaAccessibilityTable : DalgonaAccessibilityElement <NSAccessibilityTable>
@end
@implementation DalgonaAccessibilityTable
@end

static Class classFor(uint8_t role) {
	switch (role) {
	case DalgonaRoleButton:
	case DalgonaRoleLink:
	case DalgonaRoleMenuItem:
		return [DalgonaAccessibilityButton class];
	case DalgonaRoleCheckBox:
		return [DalgonaAccessibilityCheckBox class];
	case DalgonaRoleRadioButton:
		return [DalgonaAccessibilityRadioButton class];
	case DalgonaRoleTextField:
	case DalgonaRoleTextArea:
	case DalgonaRoleStaticText:
		return [DalgonaAccessibilityText class];
	case DalgonaRoleImage:
		return [DalgonaAccessibilityImage class];
	case DalgonaRoleList:
		return [DalgonaAccessibilityList class];
	case DalgonaRoleRow:
		return [DalgonaAccessibilityRow class];
	case DalgonaRoleTable:
		return [DalgonaAccessibilityTable class];
	default:
		return [DalgonaAccessibilityGroup class];
	}
}

static BOOL selectorIn(SEL selector, const SEL *selectors, size_t count) {
	for (size_t i = 0; i < count; i++) {
		if (selector == selectors[i]) {
			return YES;
		}
	}
	return NO;
}

@implementation DalgonaAccessibilityElement

- (BOOL)has:(uint8_t)ability {
	return (self.nodeAbilities & ability) != 0;
}

- (BOOL)usable:(uint8_t)ability {
	return self.nodeLive && [self has:ability] && ![self has:DalgonaDisabled];
}

- (BOOL)isAccessibilityElement {
	return self.nodeLive;
}

- (NSAccessibilityRole)accessibilityRole {
	return roleFor(self.nodeRole);
}

- (NSAccessibilitySubrole)accessibilitySubrole {
	return subroleFor(self.nodeSubrole);
}

- (NSString *)accessibilityLabel {
	return self.nodeLabel.length > 0 ? self.nodeLabel : nil;
}

- (NSString *)accessibilityHelp {
	return self.nodeHelp.length > 0 ? self.nodeHelp : nil;
}

- (NSString *)accessibilityPlaceholderValue {
	return self.nodePlaceholder.length > 0 ? self.nodePlaceholder : nil;
}

- (BOOL)isAccessibilitySelected {
	return (self.nodeStates & DalgonaSelected) != 0;
}

- (BOOL)isAccessibilityExpanded {
	return (self.nodeStates & DalgonaExpanded) != 0;
}

- (void)setAccessibilityExpanded:(BOOL)expanded {
	if (![self usable:DalgonaPressable] || (self.nodeStates & DalgonaExpandable) == 0 || expanded == [self isAccessibilityExpanded]) {
		return;
	}
	[self.nodeBridge send:DalgonaActionActivate element:self range:NSMakeRange(0, 0) value:nil];
}

- (BOOL)textAvailable {
	if ((self.nodeStates & DalgonaTextUncached) != 0) {
		[self.nodeBridge send:DalgonaActionLoadText element:self range:NSMakeRange(0, 0) value:nil];
	}
	return (self.nodeStates & (DalgonaTextUncached | DalgonaTextLoading | DalgonaTextFailed)) == 0;
}

- (id)accessibilityValue {
	if (![self textAvailable]) {
		return nil;
	}
	if ([self has:DalgonaCheckable]) {
		return @([self has:DalgonaChecked] ? 1 : 0);
	}
	if (self.nodeRole == DalgonaRoleHeading && ![roleFor(DalgonaRoleHeading) isEqualToString:NSAccessibilityGroupRole]) {
		return @(self.nodeLevel);
	}
	if (self.nodeSubrole == DalgonaSubroleSecureTextField) {
		return nil;
	}
	if ([self has:DalgonaText]) {
		return self.nodeText;
	}
	return self.nodeText.length > 0 ? self.nodeText : nil;
}

- (BOOL)isAccessibilityEnabled {
	return ![self has:DalgonaDisabled];
}

- (BOOL)isAccessibilityFocused {
	return self.nodeLive && [self.nodeBridge focusedElement] == self;
}

- (id)accessibilityParent {
	return self.nodeLive ? self.nodeParent : nil;
}

- (NSArray *)accessibilityChildren {
	return self.nodeLive ? self.nodeChildren : @[];
}

- (NSRect)accessibilityFrame {
	return self.nodeLive ? [self.nodeBridge screenFrame:self.nodePixels] : NSZeroRect;
}

- (id)accessibilityWindow {
	return self.nodeBridge.view.window;
}

- (id)accessibilityTopLevelUIElement {
	return self.nodeBridge.view.window;
}

- (id)accessibilityFocusedUIElement {
	return self;
}

- (id)accessibilityHitTest:(NSPoint)point {
	BOOL hasBounds = !NSIsEmptyRect(self.nodePixels);
	if (!self.nodeLive || (hasBounds && !NSPointInRect(point, self.accessibilityFrame))) {
		return nil;
	}
	for (DalgonaAccessibilityElement *child in [self.nodeChildren reverseObjectEnumerator]) {
		id hit = [child accessibilityHitTest:point];
		if (hit != nil) {
			return hit;
		}
	}
	return hasBounds ? self : nil;
}

- (NSArray *)accessibilityLinkedUIElements {
	return self.nodeLinked.count > 0 ? self.nodeLinked : nil;
}

- (NSArray *)accessibilityRows {
	return self.nodeRows;
}

- (NSArray *)accessibilityColumnHeaderUIElements {
	return self.nodeHeaders.count > 0 ? self.nodeHeaders : nil;
}

- (NSArray *)accessibilityRowHeaderUIElements {
	return self.nodeRowHeaders.count > 0 ? self.nodeRowHeaders : nil;
}

- (NSInteger)accessibilityRowCount {
	return (NSInteger)self.nodeRows.count;
}

- (NSInteger)accessibilityColumnCount {
	return self.nodeColumnCount;
}

- (NSInteger)accessibilityIndex {
	return self.nodeIndex;
}

- (NSRange)accessibilityRowIndexRange {
	return self.nodeRowRange;
}

- (NSRange)accessibilityColumnIndexRange {
	return self.nodeColumnRange;
}

- (BOOL)accessibilityPerformPress {
	if (![self usable:DalgonaPressable]) {
		return NO;
	}
	return [self.nodeBridge send:DalgonaActionActivate element:self range:NSMakeRange(0, 0) value:nil];
}

- (void)setAccessibilityValue:(id)value {
	if (![self usable:DalgonaEditable] || ![value isKindOfClass:[NSString class]]) {
		return;
	}
	[self.nodeBridge send:DalgonaActionSetValue element:self range:NSMakeRange(0, 0) value:value];
}

- (void)setAccessibilityFocused:(BOOL)focused {
	if (!focused || !self.nodeLive || [self has:DalgonaDisabled]) {
		return;
	}
	[self.nodeBridge send:DalgonaActionFocus element:self range:NSMakeRange(0, 0) value:nil];
}

- (NSRange)groupSelection {
	NSUInteger offset = 0;
	NSUInteger start = NSNotFound;
	NSUInteger end = 0;
	for (DalgonaAccessibilityElement *part in self.nodeTextParts) {
		if ((part.nodeStates & (DalgonaTextUncached | DalgonaTextLoading | DalgonaTextFailed)) != 0) {
			break;
		}
		offset += part.nodeTextBefore.length;
		NSRange selection = part.nodeSelection;
		if (selection.location != NSNotFound && selection.length > 0) {
			if (start == NSNotFound) {
				start = offset + selection.location;
			}
			end = offset + NSMaxRange(selection);
		}
		offset += part.nodeText.length;
	}
	return start == NSNotFound ? NSMakeRange(0, 0) : NSMakeRange(start, end - start);
}

static NSUInteger utf8Width(unichar unit) {
	if (unit < 0x80) {
		return 1;
	}
	if (unit < 0x800) {
		return 2;
	}
	return unit >= 0xd800 && unit <= 0xdbff ? 4 : 3;
}

static BOOL appendGroupText(NSMutableString *out, NSString *text, NSRange *range, NSUInteger *budget) {
	if (range->location >= text.length) {
		range->location -= text.length;
		return YES;
	}
	NSUInteger first = range->location;
	NSUInteger end = first + MIN(range->length, text.length - first);
	NSUInteger at = first;
	while (at < end) {
		unichar unit = [text characterAtIndex:at];
		NSUInteger width = utf8Width(unit);
		if (width > *budget) {
			at = MAX(first, [text rangeOfComposedCharacterSequenceAtIndex:at].location);
			break;
		}
		if ((unit >= 0xdc00 && unit <= 0xdfff) || (width == 4 && end - at == 1)) {
			return NO;
		}
		at += width == 4 ? 2 : 1;
		*budget -= width;
	}
	[out appendString:[text substringWithRange:NSMakeRange(first, at - first)]];
	range->location = 0;
	range->length -= (at - first);
	return YES;
}

- (NSString *)groupStringForRange:(NSRange)range {
	NSMutableString *out = [NSMutableString string];
	NSUInteger budget = DalgonaTextObjectLimit;
	for (DalgonaAccessibilityElement *part in self.nodeTextParts) {
		if (range.length == 0) {
			break;
		}
		if (!appendGroupText(out, part.nodeTextBefore, &range, &budget)) {
			return nil;
		}
		if (budget == 0) {
			return range.length == 0 ? out : nil;
		}
		if (range.length > 0 && (![part textAvailable] || !appendGroupText(out, part.nodeText, &range, &budget))) {
			return nil;
		}
	}
	return range.length == 0 ? out : nil;
}

- (void)setAccessibilitySelectedTextRange:(NSRange)range {
	if (![self usable:DalgonaText] || ![self textAvailable]) {
		return;
	}
	if ((self.nodeStates & DalgonaTextContainer) != 0) {
		if ((self.nodeStates & DalgonaTextSelectable) != 0 && range.location != NSNotFound) {
			[self.nodeBridge send:DalgonaActionSelectText element:self range:range value:nil];
		}
		return;
	}
	[self.nodeBridge send:DalgonaActionSelectText element:self range:range value:nil];
}

- (BOOL)validRange:(NSRange)range {
	return range.location != NSNotFound && range.location <= self.nodeText.length && range.length <= self.nodeText.length - range.location;
}

- (NSRange)selectedRange {
	return [self validRange:self.nodeSelection] ? self.nodeSelection : NSMakeRange(0, 0);
}

- (NSInteger)accessibilityNumberOfCharacters {
	if ((self.nodeStates & DalgonaTextContainer) != 0) {
		NSUInteger length = 0;
		for (DalgonaAccessibilityElement *part in self.nodeTextParts) {
			if (![part textAvailable]) {
				return NSNotFound;
			}
			length += part.nodeTextBefore.length + part.nodeText.length;
		}
		return (NSInteger)length;
	}
	return [self textAvailable] ? (NSInteger)self.nodeText.length : NSNotFound;
}

- (NSString *)accessibilitySelectedText {
	return [self accessibilityStringForRange:[self accessibilitySelectedTextRange]];
}

- (NSRange)accessibilitySelectedTextRange {
	if ((self.nodeStates & DalgonaTextContainer) != 0) {
		return [self groupSelection];
	}
	return [self textAvailable] ? [self selectedRange] : NSMakeRange(NSNotFound, 0);
}

- (NSArray<NSValue *> *)accessibilitySelectedTextRanges {
	NSRange range = [self accessibilitySelectedTextRange];
	return range.location != NSNotFound ? @[[NSValue valueWithRange:range]] : nil;
}

- (void)eachTextPart:(BOOL (^)(DalgonaAccessibilityElement *part, NSUInteger offset))visit {
	NSUInteger offset = 0;
	for (DalgonaAccessibilityElement *part in self.nodeTextParts) {
		if (![part textAvailable]) {
			return;
		}
		offset += part.nodeTextBefore.length;
		if (!visit(part, offset)) {
			return;
		}
		offset += part.nodeText.length;
	}
}

- (NSRange)groupVisibleRange {
	__block NSUInteger start = NSNotFound;
	__block NSUInteger end = 0;
	[self eachTextPart:^BOOL(DalgonaAccessibilityElement *part, NSUInteger offset) {
		if (!NSIsEmptyRect(part.nodePixels)) {
			if (start == NSNotFound) {
				start = offset;
			}
			end = offset + part.nodeText.length;
		}
		return YES;
	}];
	return start == NSNotFound ? NSMakeRange(NSNotFound, 0) : NSMakeRange(start, end - start);
}

- (NSRange)accessibilityVisibleCharacterRange {
	if ((self.nodeStates & DalgonaTextContainer) != 0) {
		return NSIsEmptyRect(self.nodePixels) ? NSMakeRange(NSNotFound, 0) : [self groupVisibleRange];
	}
	return [self textAvailable] ? NSMakeRange(0, self.nodeText.length) : NSMakeRange(NSNotFound, 0);
}

- (NSString *)accessibilityStringForRange:(NSRange)range {
	if ((self.nodeStates & DalgonaTextContainer) != 0) {
		if (range.location == NSNotFound || range.length > NSUIntegerMax - range.location) {
			return nil;
		}
		return [self groupStringForRange:range];
	}
	return [self textAvailable] && [self validRange:range] ? [self.nodeText substringWithRange:range] : nil;
}

- (NSAttributedString *)accessibilityAttributedStringForRange:(NSRange)range {
	NSString *text = [self accessibilityStringForRange:range];
	return text != nil ? [[NSAttributedString alloc] initWithString:text] : nil;
}

- (NSUInteger)lineCount {
	return self.nodeLines.length / 4;
}

- (NSUInteger)lineStart:(NSUInteger)line {
	return MIN((NSUInteger)readU32((const uint8_t *)self.nodeLines.bytes + 4 * line), self.nodeText.length);
}

- (NSUInteger)lineEnd:(NSUInteger)line {
	return line + 1 < [self lineCount] ? [self lineStart:line + 1] : self.nodeText.length;
}

- (void)eachBox:(void (^)(const DalgonaBox *box, NSUInteger start, NSUInteger end))visit {
	NSData *boxes = self.nodeBoxes;
	const uint8_t *p = boxes.bytes;
	const uint8_t *last = p + boxes.length;
	while (p != NULL && p < last) {
		DalgonaBox box;
		p = readBox(p, &box);
		NSUInteger start = [self lineStart:box.line];
		visit(&box, start, MAX([self lineEnd:box.line], start));
	}
}

- (NSInteger)accessibilityLineForIndex:(NSInteger)index {
	NSUInteger count = [self lineCount];
	if (![self textAvailable] || index < 0 || (NSUInteger)index > self.nodeText.length || count == 0) {
		return NSNotFound;
	}
	NSUInteger low = 0;
	NSUInteger high = count;
	while (high - low > 1) {
		NSUInteger middle = low + (high - low) / 2;
		if ([self lineStart:middle] <= (NSUInteger)index) {
			low = middle;
		} else {
			high = middle;
		}
	}
	return (NSInteger)low;
}

- (NSRange)accessibilityRangeForLine:(NSInteger)line {
	if (![self textAvailable] || line < 0 || (NSUInteger)line >= [self lineCount]) {
		return NSMakeRange(NSNotFound, 0);
	}
	NSUInteger start = [self lineStart:(NSUInteger)line];
	return NSMakeRange(start, MAX([self lineEnd:(NSUInteger)line], start) - start);
}

- (NSInteger)accessibilityInsertionPointLineNumber {
	return [self accessibilityLineForIndex:(NSInteger)[self selectedRange].location];
}

- (BOOL)caretAtLineEnd:(NSUInteger)index pixels:(NSRect *)pixels {
	NSInteger line = [self accessibilityLineForIndex:(NSInteger)index];
	__block BOOL found = NO;
	[self eachBox:^(const DalgonaBox *box, NSUInteger start, NSUInteger end) {
		if (found || (NSInteger)box->line != line) {
			return;
		}
		CGFloat x = NSMinX(box->pixels);
		if (box->count > 0) {
			int32_t last = clusterLeft(box, box->count - 1);
			x = last < clusterLeft(box, 0) ? last : clusterRight(box, box->count - 1);
		}
		*pixels = clusterPixels(box, x, x);
		found = YES;
	}];
	return found;
}

- (NSRect)groupFrameForRange:(NSRange)range {
	if (NSIsEmptyRect(self.nodePixels) || range.location == NSNotFound || range.length > NSUIntegerMax - range.location) {
		return NSZeroRect;
	}
	NSUInteger end = NSMaxRange(range);
	__block NSRect frame = NSZeroRect;
	__block BOOL found = NO;
	[self eachTextPart:^BOOL(DalgonaAccessibilityElement *part, NSUInteger offset) {
		if (end < offset) {
			return NO;
		}
		NSUInteger from = MAX(range.location, offset);
		NSUInteger to = MIN(end, offset + part.nodeText.length);
		if (!NSIsEmptyRect(part.nodePixels) && (from < to || (from == to && range.length == 0))) {
			NSRect local = [part accessibilityFrameForRange:NSMakeRange(from - offset, to - from)];
			frame = found ? coverPixels(frame, local) : local;
			found = YES;
		}
		return offset + part.nodeText.length < end;
	}];
	return frame;
}

- (NSRect)accessibilityFrameForRange:(NSRange)range {
	if ((self.nodeStates & DalgonaTextContainer) != 0) {
		return [self groupFrameForRange:range];
	}
	if (!self.nodeLive || ![self textAvailable] || ![self validRange:range]) {
		return NSZeroRect;
	}
	if (self.nodeBoxes.length == 0) {
		return self.accessibilityFrame;
	}
	NSUInteger first = range.location;
	NSUInteger last = MAX(NSMaxRange(range), first + 1);
	__block BOOL found = NO;
	__block NSRect pixels = NSZeroRect;
	[self eachBox:^(const DalgonaBox *box, NSUInteger start, NSUInteger end) {
		for (uint32_t k = 0; k < box->count; k++) {
			NSUInteger from = clusterStart(box, k);
			NSUInteger to = k + 1 < box->count ? clusterStart(box, k + 1) : end;
			if (from >= last || to <= first) {
				continue;
			}
			NSRect cluster = clusterPixels(box, clusterLeft(box, k), clusterRight(box, k));
			pixels = found ? coverPixels(pixels, cluster) : cluster;
			found = YES;
		}
	}];
	if (!found && range.length == 0) {
		found = [self caretAtLineEnd:first pixels:&pixels];
	}
	return found ? [self.nodeBridge screenFrame:pixels] : NSZeroRect;
}

- (NSRange)groupRangeForPosition:(NSPoint)point {
	__block NSRange hit = NSMakeRange(NSNotFound, 0);
	if (!NSPointInRect(point, self.accessibilityFrame)) {
		return hit;
	}
	[self eachTextPart:^BOOL(DalgonaAccessibilityElement *part, NSUInteger offset) {
		if (!NSPointInRect(point, part.accessibilityFrame)) {
			return YES;
		}
		hit = [part accessibilityRangeForPosition:point];
		if (hit.location == NSNotFound) {
			return YES;
		}
		hit.location += offset;
		return NO;
	}];
	return hit;
}

- (NSRange)accessibilityRangeForPosition:(NSPoint)point {
	if ((self.nodeStates & DalgonaTextContainer) != 0) {
		return [self groupRangeForPosition:point];
	}
	if (![self textAvailable]) {
		return NSMakeRange(NSNotFound, 0);
	}
	NSPoint pixel = NSZeroPoint;
	if (!self.nodeLive || ![self.nodeBridge pixelPoint:point into:&pixel]) {
		return NSMakeRange(NSNotFound, 0);
	}
	CGFloat x = floor(pixel.x);
	NSPoint probe = NSMakePoint(x, floor(pixel.y));
	__block NSRange hit = NSMakeRange(NSNotFound, 0);
	[self eachBox:^(const DalgonaBox *box, NSUInteger start, NSUInteger end) {
		if (hit.location != NSNotFound || !NSPointInRect(probe, box->pixels)) {
			return;
		}
		hit = NSMakeRange(start, 0);
		CGFloat distance = CGFLOAT_MAX;
		for (uint32_t k = 0; k < box->count; k++) {
			CGFloat gap = MAX(MAX(clusterLeft(box, k) - x, x - clusterRight(box, k) + 1), 0);
			if (gap < distance) {
				NSUInteger from = clusterStart(box, k);
				NSUInteger to = k + 1 < box->count ? clusterStart(box, k + 1) : end;
				hit = NSMakeRange(from, MAX(to, from) - from);
				distance = gap;
			}
		}
	}];
	return hit;
}

- (NSRange)groupRangeForIndex:(NSUInteger)index {
	__block NSRange range = NSMakeRange(NSNotFound, 0);
	[self eachTextPart:^BOOL(DalgonaAccessibilityElement *part, NSUInteger offset) {
		NSUInteger prefix = offset - part.nodeTextBefore.length;
		if (index < offset) {
			range = [part.nodeTextBefore rangeOfComposedCharacterSequenceAtIndex:index - prefix];
			range.location += prefix;
			return NO;
		}
		if (index - offset >= part.nodeText.length) {
			return YES;
		}
		range = [part.nodeText rangeOfComposedCharacterSequenceAtIndex:index - offset];
		range.location += offset;
		return NO;
	}];
	return range;
}

- (NSRange)accessibilityRangeForIndex:(NSInteger)index {
	if ((self.nodeStates & DalgonaTextContainer) != 0 && index >= 0) {
		return [self groupRangeForIndex:(NSUInteger)index];
	}
	if (![self textAvailable] || index < 0 || (NSUInteger)index >= self.nodeText.length) {
		return NSMakeRange(NSNotFound, 0);
	}
	return [self.nodeText rangeOfComposedCharacterSequenceAtIndex:(NSUInteger)index];
}

- (NSRange)accessibilityStyleRangeForIndex:(NSInteger)index {
	if ((self.nodeStates & DalgonaTextContainer) != 0 && index >= 0) {
		return [self groupRangeForIndex:(NSUInteger)index];
	}
	if (![self textAvailable] || index < 0 || (NSUInteger)index >= self.nodeText.length) {
		return NSMakeRange(NSNotFound, 0);
	}
	return NSMakeRange(0, self.nodeText.length);
}

- (BOOL)isAccessibilitySelectorAllowed:(SEL)selector {
	const SEL text[] = {
		@selector(accessibilityNumberOfCharacters), @selector(accessibilitySelectedText),
		@selector(accessibilitySelectedTextRange), @selector(accessibilitySelectedTextRanges),
		@selector(accessibilityVisibleCharacterRange), @selector(accessibilityStringForRange:),
		@selector(accessibilityAttributedStringForRange:), @selector(accessibilityLineForIndex:),
		@selector(accessibilityRangeForLine:), @selector(accessibilityInsertionPointLineNumber),
		@selector(accessibilityFrameForRange:), @selector(accessibilityRangeForIndex:),
		@selector(accessibilityStyleRangeForIndex:), @selector(setAccessibilitySelectedTextRange:),
		@selector(accessibilityRangeForPosition:),
	};
	const SEL table[] = {@selector(accessibilityRows), @selector(accessibilityRowCount), @selector(accessibilityColumnCount)};
	const SEL headers[] = {@selector(accessibilityColumnHeaderUIElements), @selector(accessibilityRowHeaderUIElements)};
	const SEL cell[] = {@selector(accessibilityRowIndexRange), @selector(accessibilityColumnIndexRange)};
	if (selector == @selector(accessibilityPerformPress)) {
		return [self usable:DalgonaPressable];
	}
	if (selector == @selector(setAccessibilityValue:)) {
		return [self usable:DalgonaEditable];
	}
	if (selector == @selector(setAccessibilityFocused:)) {
		return self.nodeLive && ![self has:DalgonaDisabled] && [self has:DalgonaPressable | DalgonaEditable | DalgonaText];
	}
	if (selector == @selector(isAccessibilitySelected)) {
		return (self.nodeStates & DalgonaSelectable) != 0;
	}
	if (selector == @selector(isAccessibilityExpanded) || selector == @selector(setAccessibilityExpanded:)) {
		return (self.nodeStates & DalgonaExpandable) != 0;
	}
	if (selector == @selector(accessibilityPlaceholderValue)) {
		return self.nodePlaceholder.length > 0;
	}
	if (selectorIn(selector, text, sizeof(text) / sizeof(text[0]))) {
		return self.nodeLive && [self has:DalgonaText];
	}
	if (selectorIn(selector, table, sizeof(table) / sizeof(table[0]))) {
		return self.nodeRole == DalgonaRoleTable || self.nodeRole == DalgonaRoleList;
	}
	if (selectorIn(selector, headers, sizeof(headers) / sizeof(headers[0]))) {
		return self.nodeRole == DalgonaRoleTable || self.nodeRole == DalgonaRoleCell;
	}
	if (selectorIn(selector, cell, sizeof(cell) / sizeof(cell[0]))) {
		return self.nodeRole == DalgonaRoleCell;
	}
	if (selector == @selector(accessibilityIndex)) {
		return self.nodeRole == DalgonaRoleRow;
	}
	return [super isAccessibilitySelectorAllowed:selector];
}

@end

static char DalgonaBridgeKey;
static NSString *const DalgonaAccessiblePrefix = @"DalgonaAccessible_";

static BOOL isAccessibleClass(Class c) {
	return [NSStringFromClass(c) hasPrefix:DalgonaAccessiblePrefix];
}

static DalgonaAccessibilityBridge *bridgeOf(id view) {
	return objc_getAssociatedObject(view, &DalgonaBridgeKey);
}

static IMP inheritedImplementation(id view, SEL selector) {
	Class c = object_getClass(view);
	while (c != Nil && !isAccessibleClass(c)) {
		c = class_getSuperclass(c);
	}
	return class_getMethodImplementation(c != Nil ? class_getSuperclass(c) : [NSView class], selector);
}

static NSArray *viewChildren(id self, SEL _cmd) {
	NSArray *children = [bridgeOf(self) rootChildren];
	if (children != nil) {
		return children;
	}
	return ((NSArray * (*)(id, SEL))inheritedImplementation(self, _cmd))(self, _cmd);
}

static id viewFocusedElement(id self, SEL _cmd) {
	id focused = [bridgeOf(self) focusedElement];
	if (focused != nil) {
		return focused;
	}
	return ((id(*)(id, SEL))inheritedImplementation(self, _cmd))(self, _cmd);
}

static id viewHitTest(id self, SEL _cmd, NSPoint point) {
	id hit = [bridgeOf(self) hitTest:point];
	if (hit != nil) {
		return hit;
	}
	return ((id(*)(id, SEL, NSPoint))inheritedImplementation(self, _cmd))(self, _cmd, point);
}

static void addOverride(Class subclass, SEL selector, IMP implementation, NSString *types) {
	Method inherited = class_getInstanceMethod(subclass, selector);
	class_addMethod(subclass, selector, implementation, inherited != NULL ? method_getTypeEncoding(inherited) : types.UTF8String);
}

static Class accessibleClass(Class original) {
	if (isAccessibleClass(original)) {
		return original;
	}
	NSString *name = [DalgonaAccessiblePrefix stringByAppendingString:NSStringFromClass(original)];
	Class existing = NSClassFromString(name);
	if (existing != Nil) {
		return existing;
	}
	Class subclass = objc_allocateClassPair(original, name.UTF8String, 0);
	if (subclass == Nil) {
		return Nil;
	}
	NSString *query = [NSString stringWithFormat:@"%s%s%s", @encode(id), @encode(id), @encode(SEL)];
	addOverride(subclass, @selector(accessibilityChildren), (IMP)viewChildren, query);
	addOverride(subclass, @selector(accessibilityFocusedUIElement), (IMP)viewFocusedElement, query);
	addOverride(subclass, @selector(accessibilityHitTest:), (IMP)viewHitTest, [query stringByAppendingFormat:@"%s", @encode(NSPoint)]);
	objc_registerClassPair(subclass);
	return subclass;
}

static BOOL readLines(const uint8_t *base, uint64_t size, uint64_t *offset, DalgonaNode *n) {
	if (*offset + 4 * (uint64_t)n->lineCount > size) {
		return NO;
	}
	n->lines = base + *offset;
	uint32_t previous = 0;
	for (uint32_t k = 0; k < n->lineCount; k++) {
		uint32_t start = readU32(n->lines + 4 * (uint64_t)k);
		if (start < previous || start > n->units[2] || (k == 0 && start != 0)) {
			return NO;
		}
		previous = start;
	}
	*offset += 4 * (uint64_t)n->lineCount;
	n->boxes = base + *offset;
	uint64_t first = *offset;
	for (uint32_t k = 0; k < n->boxCount; k++) {
		if (*offset + DalgonaBoxHeader > size) {
			return NO;
		}
		const uint8_t *box = base + *offset;
		uint64_t clusters = readU32(box + 20);
		*offset += DalgonaBoxHeader;
		if (readU32(box) >= n->lineCount || *offset + DalgonaClusterSize * clusters > size) {
			return NO;
		}
		*offset += DalgonaClusterSize * clusters;
	}
	n->boxBytes = *offset - first;
	return YES;
}

static BOOL readNodes(NSData *snapshot, uint32_t count, DalgonaNode *nodes) {
	const uint8_t *base = snapshot.bytes;
	uint64_t size = snapshot.length;
	uint64_t offset = DalgonaSnapshotHeader;
	for (uint32_t i = 0; i < count; i++) {
		if (offset + DalgonaNodeHeader > size) {
			return NO;
		}
		const uint8_t *p = base + offset;
		DalgonaNode *n = &nodes[i];
		n->parent = readU32(p);
		n->target = readU32(p + 4);
		n->role = p[8];
		n->subrole = p[9];
		n->abilities = p[10];
		n->level = p[11];
		n->pixels = readPixels(p + 12);
		n->location = readU32(p + 28);
		n->length = readU32(p + 32);
		n->identityLength = readU32(p + 36);
		n->row = readU32(p + 52);
		n->rows = readU32(p + 56);
		n->column = readU32(p + 60);
		n->columns = readU32(p + 64);
		n->lineCount = readU32(p + 68);
		n->boxCount = readU32(p + 72);
		n->units[3] = readU32(p + 76);
		n->states = readU32(p + 80);
		n->textGroup = readU32(p + 84);
		n->textIndex = readU32(p + 88);
		n->units[4] = readU32(p + 92);
		offset += DalgonaNodeHeader;
		if (offset + n->identityLength > size) {
			return NO;
		}
		n->identity = base + offset;
		offset += n->identityLength;
		for (int k = 0; k < 5; k++) {
			if (k < 3) {
				n->units[k] = readU32(p + 40 + 4 * k);
			}
			if (offset + 2 * (uint64_t)n->units[k] > size) {
				return NO;
			}
			n->texts[k] = base + offset;
			offset += 2 * (uint64_t)n->units[k];
		}
		if (!readLines(base, size, &offset, n)) {
			return NO;
		}
		BOOL parentValid = i == 0 ? n->parent == DalgonaNone : n->parent < i;
		if (!parentValid || (n->target != DalgonaNone && n->target >= count) || (n->textGroup != DalgonaNone && n->textGroup >= i)) {
			return NO;
		}
	}
	return offset == size;
}

static NSRange headerAxis(DalgonaAccessibilityElement *cell, uint8_t header) {
	return header == DalgonaRowHeader ? cell.nodeRowRange : cell.nodeColumnRange;
}

static NSArray *headersIn(NSArray<DalgonaAccessibilityElement *> *cells, uint8_t header, DalgonaAccessibilityElement *cell) {
	NSMutableArray *headers = [NSMutableArray array];
	for (DalgonaAccessibilityElement *candidate in cells) {
		if (candidate == cell || ![candidate has:header]) {
			continue;
		}
		if (cell == nil || NSIntersectionRange(headerAxis(candidate, header), headerAxis(cell, header)).length > 0) {
			[headers addObject:candidate];
		}
	}
	return headers;
}

@implementation DalgonaAccessibilityBridge {
	int _actions;
	dispatch_queue_t _writer;
	BOOL _closed;
	NSData *_pending;
	NSUInteger _inflight;
	NSMutableDictionary<NSData *, DalgonaAccessibilityElement *> *_elements;
	DalgonaAccessibilityElement *_root;
	DalgonaAccessibilityElement *_focus;
	uint64_t _revision;
}

- (instancetype)initWithView:(NSView *)view actions:(int)actions {
	self = [super init];
	if (self != nil) {
		_view = view;
		_actions = actions;
		_writer = dispatch_queue_create("com.gosuda.dalgona.accessibility.actions", DISPATCH_QUEUE_SERIAL);
		_elements = [NSMutableDictionary dictionary];
	}
	return self;
}

- (void)install {
	NSView *view = self.view;
	@synchronized(self) {
		if (_closed || view == nil) {
			return;
		}
	}
	Class accessible = accessibleClass(object_getClass(view));
	if (accessible == Nil) {
		return;
	}
	objc_setAssociatedObject(view, &DalgonaBridgeKey, self, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
	object_setClass(view, accessible);
}

- (void)uninstall {
	NSView *view = self.view;
	if (view != nil && bridgeOf(view) == self) {
		objc_setAssociatedObject(view, &DalgonaBridgeKey, nil, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
		Class current = object_getClass(view);
		if (isAccessibleClass(current)) {
			object_setClass(view, class_getSuperclass(current));
		}
	}
	NSArray<DalgonaAccessibilityElement *> *removed = _elements.allValues;
	[_elements removeAllObjects];
	_root = nil;
	_focus = nil;
	[self destroy:removed];
	if (view != nil) {
		NSAccessibilityPostNotification(view, NSAccessibilityLayoutChangedNotification);
	}
}

- (void)destroy:(NSArray<DalgonaAccessibilityElement *> *)elements {
	for (DalgonaAccessibilityElement *element in elements) {
		element.nodeLive = NO;
		element.nodeParent = nil;
		element.nodeChildren = @[];
		element.nodeLinked = nil;
		element.nodeRows = nil;
		element.nodeHeaders = nil;
		element.nodeRowHeaders = nil;
		NSAccessibilityPostNotification(element, NSAccessibilityUIElementDestroyedNotification);
	}
}

- (void)publish:(NSData *)snapshot {
	BOOL schedule = NO;
	@synchronized(self) {
		if (_closed) {
			return;
		}
		schedule = _pending == nil;
		_pending = snapshot;
	}
	if (schedule) {
		dispatch_async(dispatch_get_main_queue(), ^{
			[self applyPending];
		});
	}
}

- (void)applyPending {
	NSData *snapshot = nil;
	@synchronized(self) {
		snapshot = _pending;
		_pending = nil;
		if (_closed) {
			return;
		}
	}
	if (snapshot.length < DalgonaSnapshotHeader) {
		return;
	}
	const uint8_t *p = snapshot.bytes;
	uint32_t count = readU32(p + 8);
	uint32_t focus = readU32(p + 12);
	if (count == 0 || count > (snapshot.length - DalgonaSnapshotHeader) / DalgonaNodeHeader || (focus != DalgonaNone && focus >= count)) {
		return;
	}
	NSMutableData *storage = [NSMutableData dataWithLength:(NSUInteger)count * sizeof(DalgonaNode)];
	DalgonaNode *nodes = storage.mutableBytes;
	if (!readNodes(snapshot, count, nodes)) {
		return;
	}
	[self apply:nodes count:count revision:readU64(p) focus:focus];
}

- (void)apply:(const DalgonaNode *)nodes count:(uint32_t)count revision:(uint64_t)revision focus:(uint32_t)focus {
	NSView *view = self.view;
	BOOL initial = _root == nil;
	NSMutableDictionary<NSData *, DalgonaAccessibilityElement *> *previous = _elements;
	NSMutableDictionary<NSData *, DalgonaAccessibilityElement *> *current = [NSMutableDictionary dictionaryWithCapacity:count];
	NSMutableArray<DalgonaAccessibilityElement *> *order = [NSMutableArray arrayWithCapacity:count];
	NSMutableArray<NSMutableArray *> *children = [NSMutableArray arrayWithCapacity:count];
	NSMutableArray<NSMutableArray *> *linked = [NSMutableArray arrayWithCapacity:count];
	NSMutableDictionary<NSNumber *, NSMutableArray<DalgonaAccessibilityElement *> *> *textParts = [NSMutableDictionary dictionary];
	NSMutableArray *created = [NSMutableArray array];
	NSMutableArray *retitled = [NSMutableArray array];
	NSMutableOrderedSet *revalued = [NSMutableOrderedSet orderedSet];
	NSMutableOrderedSet *reselected = [NSMutableOrderedSet orderedSet];
	BOOL restructured = NO;
	for (uint32_t i = 0; i < count; i++) {
		const DalgonaNode *n = &nodes[i];
		NSData *identity = [NSData dataWithBytes:n->identity length:n->identityLength];
		DalgonaAccessibilityElement *element = previous[identity];
		Class cls = classFor(n->role);
		BOOL fresh = element == nil || object_getClass(element) != cls || current[identity] != nil;
		if (fresh) {
			element = [[cls alloc] init];
			element.nodeBridge = self;
			element.nodeIdentity = identity;
			[created addObject:element];
		} else {
			[previous removeObjectForKey:identity];
		}
		current[identity] = element;
		NSString *label = readText(n->texts[0], n->units[0]);
		NSString *help = readText(n->texts[1], n->units[1]);
		NSString *text = readText(n->texts[2], n->units[2]);
		NSString *prefix = readText(n->texts[4], n->units[4]);
		DalgonaAccessibilityElement *group = n->textGroup == DalgonaNone ? nil : order[n->textGroup];
		NSRange selection = NSMakeRange(n->location == DalgonaNone ? NSNotFound : n->location, n->length);
		id parent = i == 0 ? view : order[n->parent];
		if (!fresh) {
			if (![element.nodeLabel isEqualToString:label]) {
				[retitled addObject:element];
			}
			if (![element.nodeText isEqualToString:text] || ![element.nodeTextBefore isEqualToString:prefix] ||
				((element.nodeAbilities ^ n->abilities) & DalgonaChecked) != 0 || element.nodeStates != n->states) {
				[revalued addObject:element];
				if (group != nil) {
					[revalued addObject:group];
				}
			}
			if (!NSEqualRanges(element.nodeSelection, selection)) {
				[reselected addObject:element];
				if (group != nil) {
					[reselected addObject:group];
				}
			}
			restructured = restructured || element.nodeParent != parent;
		}
		element.nodeLive = YES;
		element.nodeRevision = revision;
		element.nodeRole = n->role;
		element.nodeSubrole = n->subrole;
		element.nodeAbilities = n->abilities;
		element.nodeLevel = n->level;
		element.nodePixels = n->pixels;
		element.nodeLabel = label;
		element.nodeHelp = help;
		element.nodeText = text;
		element.nodePlaceholder = readText(n->texts[3], n->units[3]);
		element.nodeTextBefore = prefix;
		element.nodeTextIndex = n->textIndex;
		element.nodeTextParts = nil;
		element.nodeStates = n->states;
		element.nodeSelection = selection;
		element.nodeParent = parent;
		element.nodeIndex = 0;
		element.nodeRowRange = n->row == DalgonaNone ? NSMakeRange(NSNotFound, 0) : NSMakeRange(n->row, n->rows);
		element.nodeColumnRange = n->column == DalgonaNone ? NSMakeRange(NSNotFound, 0) : NSMakeRange(n->column, n->columns);
		element.nodeRows = nil;
		element.nodeHeaders = nil;
		element.nodeRowHeaders = nil;
		element.nodeColumnCount = 0;
		element.nodeLines = [NSData dataWithBytes:n->lines length:4 * (NSUInteger)n->lineCount];
		element.nodeBoxes = [NSData dataWithBytes:n->boxes length:(NSUInteger)n->boxBytes];
		[order addObject:element];
		[children addObject:[NSMutableArray array]];
		[linked addObject:[NSMutableArray array]];
		if (n->textGroup != DalgonaNone) {
			NSNumber *key = @(n->textGroup);
			NSMutableArray *parts = textParts[key];
			if (parts == nil) {
				parts = [NSMutableArray array];
				textParts[key] = parts;
			}
			[parts addObject:element];
		}
		if (i > 0) {
			[children[n->parent] addObject:element];
		}
	}
	for (uint32_t i = 0; i < count; i++) {
		if (nodes[i].target != DalgonaNone) {
			[linked[i] addObject:order[nodes[i].target]];
			[linked[nodes[i].target] addObject:order[i]];
		}
	}
	for (uint32_t i = 0; i < count; i++) {
		DalgonaAccessibilityElement *element = order[i];
		restructured = restructured || ![element.nodeChildren isEqualToArray:children[i]];
		element.nodeChildren = children[i];
		element.nodeLinked = linked[i];
	}
	[textParts enumerateKeysAndObjectsUsingBlock:^(NSNumber *key, NSMutableArray<DalgonaAccessibilityElement *> *parts, BOOL *stop) {
		[parts sortUsingComparator:^NSComparisonResult(DalgonaAccessibilityElement *a, DalgonaAccessibilityElement *b) {
			return a.nodeTextIndex < b.nodeTextIndex ? NSOrderedAscending : (a.nodeTextIndex == b.nodeTextIndex ? NSOrderedSame : NSOrderedDescending);
		}];
		order[key.unsignedIntValue].nodeTextParts = parts;
	}];
	for (DalgonaAccessibilityElement *element in order) {
		[self relateTable:element];
	}
	NSArray<DalgonaAccessibilityElement *> *removed = previous.allValues;
	DalgonaAccessibilityElement *previousFocus = _focus;
	_elements = current;
	_root = order[0];
	_focus = focus == DalgonaNone ? nil : order[focus];
	_revision = revision;
	[self destroy:removed];
	if (!initial) {
		for (DalgonaAccessibilityElement *element in created) {
			NSAccessibilityPostNotification(element, NSAccessibilityCreatedNotification);
		}
	}
	if (initial || restructured || created.count > 0 || removed.count > 0) {
		NSAccessibilityPostNotificationWithUserInfo(_root, NSAccessibilityLayoutChangedNotification, @{NSAccessibilityUIElementsKey: created});
	}
	for (DalgonaAccessibilityElement *element in retitled) {
		NSAccessibilityPostNotification(element, NSAccessibilityTitleChangedNotification);
	}
	for (DalgonaAccessibilityElement *element in revalued) {
		NSAccessibilityPostNotification(element, NSAccessibilityValueChangedNotification);
	}
	for (DalgonaAccessibilityElement *element in reselected) {
		NSAccessibilityPostNotification(element, NSAccessibilitySelectedTextChangedNotification);
	}
	if (_focus != nil && _focus != previousFocus) {
		NSAccessibilityPostNotification(_focus, NSAccessibilityFocusedUIElementChangedNotification);
	}
}

- (void)relateTable:(DalgonaAccessibilityElement *)table {
	if (table.nodeRole != DalgonaRoleTable && table.nodeRole != DalgonaRoleList) {
		return;
	}
	BOOL list = table.nodeRole == DalgonaRoleList;
	NSMutableArray *rows = [NSMutableArray array];
	NSMutableArray<DalgonaAccessibilityElement *> *cells = [NSMutableArray array];
	NSUInteger columns = 0;
	for (DalgonaAccessibilityElement *child in table.nodeChildren) {
		if (!list && child.nodeRole != DalgonaRoleRow) {
			continue;
		}
		child.nodeIndex = (NSInteger)rows.count;
		[rows addObject:child];
		for (DalgonaAccessibilityElement *cell in list ? @[] : child.nodeChildren) {
			if (cell.nodeRowRange.location != NSNotFound && cell.nodeColumnRange.location != NSNotFound) {
				[cells addObject:cell];
				columns = MAX(columns, NSMaxRange(cell.nodeColumnRange));
			}
		}
	}
	table.nodeRows = rows;
	table.nodeColumnCount = (NSInteger)(list && rows.count > 0 ? 1 : columns);
	table.nodeHeaders = headersIn(cells, DalgonaColumnHeader, nil);
	table.nodeRowHeaders = headersIn(cells, DalgonaRowHeader, nil);
	for (DalgonaAccessibilityElement *cell in cells) {
		cell.nodeHeaders = headersIn(cells, DalgonaColumnHeader, cell);
		cell.nodeRowHeaders = headersIn(cells, DalgonaRowHeader, cell);
	}
}

- (void)announce:(NSString *)text priority:(NSAccessibilityPriorityLevel)priority {
	dispatch_async(dispatch_get_main_queue(), ^{
		@synchronized(self) {
			if (self->_closed) {
				return;
			}
		}
		id target = self.view.window;
		NSDictionary *info = @{NSAccessibilityAnnouncementKey: text, NSAccessibilityPriorityKey: @(priority)};
		NSAccessibilityPostNotificationWithUserInfo(target != nil ? target : NSApp, NSAccessibilityAnnouncementRequestedNotification, info);
	});
}

- (void)detach {
	@synchronized(self) {
		if (_closed) {
			return;
		}
		_closed = YES;
		_pending = nil;
		int actions = _actions;
		dispatch_async(_writer, ^{
			close(actions);
		});
	}
	dispatch_async(dispatch_get_main_queue(), ^{
		[self uninstall];
	});
}

- (NSArray *)rootChildren {
	return _root != nil ? @[_root] : nil;
}

- (id)focusedElement {
	return _focus;
}

- (id)hitTest:(NSPoint)point {
	if (_root == nil || !NSPointInRect(point, _root.accessibilityFrame)) {
		return nil;
	}
	return [_root accessibilityHitTest:point];
}

- (NSRect)screenFrame:(NSRect)pixels {
	NSView *view = self.view;
	NSWindow *window = view.window;
	CGFloat scale = window.backingScaleFactor;
	if (window == nil || scale <= 0) {
		return NSZeroRect;
	}
	NSRect local = NSMakeRect(NSMinX(pixels) / scale, NSMinY(pixels) / scale, NSWidth(pixels) / scale, NSHeight(pixels) / scale);
	if (!view.isFlipped) {
		local.origin.y = NSHeight(view.bounds) - NSMaxY(local);
	}
	return NSAccessibilityFrameInView(view, local);
}

- (BOOL)pixelPoint:(NSPoint)point into:(NSPoint *)pixel {
	NSView *view = self.view;
	NSWindow *window = view.window;
	CGFloat scale = window.backingScaleFactor;
	if (window == nil || scale <= 0) {
		return NO;
	}
	NSPoint local = [view convertPoint:[window convertPointFromScreen:point] fromView:nil];
	if (!view.isFlipped) {
		local.y = NSHeight(view.bounds) - local.y;
	}
	*pixel = NSMakePoint(local.x * scale, local.y * scale);
	return YES;
}

- (BOOL)send:(uint8_t)action element:(DalgonaAccessibilityElement *)element range:(NSRange)range value:(NSString *)value {
	NSUInteger units = value.length;
	NSData *identity = element.nodeIdentity;
	if (!element.nodeLive || element.nodeRevision != _revision || units > DalgonaValueUnitLimit || range.location > UINT32_MAX || range.length > UINT32_MAX) {
		return NO;
	}
	NSMutableData *record = [NSMutableData dataWithLength:DalgonaActionHeader + identity.length + units * 2];
	uint8_t *p = record.mutableBytes;
	writeU64(p, element.nodeRevision);
	p[8] = action;
	writeU32(p + 12, (uint32_t)range.location);
	writeU32(p + 16, (uint32_t)range.length);
	writeU32(p + 20, (uint32_t)identity.length);
	writeU32(p + 24, (uint32_t)units);
	memcpy(p + DalgonaActionHeader, identity.bytes, identity.length);
	if (units > 0) {
		NSMutableData *characters = [NSMutableData dataWithLength:units * sizeof(unichar)];
		const unichar *c = characters.mutableBytes;
		[value getCharacters:characters.mutableBytes range:NSMakeRange(0, units)];
		uint8_t *q = p + DalgonaActionHeader + identity.length;
		for (NSUInteger i = 0; i < units; i++) {
			q[2 * i] = (uint8_t)c[i];
			q[2 * i + 1] = (uint8_t)(c[i] >> 8);
		}
	}
	@synchronized(self) {
		if (_closed || _inflight >= DalgonaActionLimit) {
			return NO;
		}
		_inflight++;
		int actions = _actions;
		dispatch_async(_writer, ^{
			writeAll(actions, record.bytes, record.length);
			@synchronized(self) {
				self->_inflight--;
			}
		});
	}
	return YES;
}

@end

static DalgonaAccessibilityBridge *bridgeFrom(uintptr_t bridge) {
	return (__bridge DalgonaAccessibilityBridge *)(void *)bridge;
}

int dalgona_ax_attach(uintptr_t view, uintptr_t *bridge, int *actions) {
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
		DalgonaAccessibilityBridge *b = [[DalgonaAccessibilityBridge alloc] initWithView:(__bridge NSView *)(void *)view actions:fds[1]];
		dispatch_async(dispatch_get_main_queue(), ^{
			[b install];
		});
		*bridge = (uintptr_t)CFBridgingRetain(b);
		*actions = fds[0];
		return 0;
	}
}

void dalgona_ax_publish(uintptr_t bridge, const uint8_t *snapshot, size_t length) {
	@autoreleasepool {
		[bridgeFrom(bridge) publish:[NSData dataWithBytes:snapshot length:length]];
	}
}

void dalgona_ax_announce(uintptr_t bridge, const uint8_t *text, size_t length, uint8_t priority) {
	@autoreleasepool {
		NSString *announcement = [[NSString alloc] initWithBytes:text length:length encoding:NSUTF16LittleEndianStringEncoding];
		if (announcement.length > 0) {
			[bridgeFrom(bridge) announce:announcement priority:priorityFor(priority)];
		}
	}
}

void dalgona_ax_detach(uintptr_t bridge) {
	@autoreleasepool {
		DalgonaAccessibilityBridge *b = CFBridgingRelease((CFTypeRef)(void *)bridge);
		[b detach];
	}
}
