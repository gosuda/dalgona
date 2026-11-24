package theme

import "time"

type (
	Dp     float32
	Sp     float32
	Weight int
)

const (
	SizeCaption   Sp = 12
	SizeSmall     Sp = 13
	SizeLabel     Sp = 14
	SizeBody      Sp = 15
	InputTextSize Sp = 16
	SizeHeading   Sp = 22
	SizeTitle     Sp = 26
	SizeDisplay   Sp = 30
)

const (
	LeadingTight       float32 = 1
	LeadingBody        float32 = 1.5
	LeadingCode        float32 = 1.4
	LeadingCardTitle   float32 = 32.0 / 26
	LeadingCommand     float32 = 38.0 / 30
	LeadingDirectory   float32 = 20.0 / 13
	LeadingReason      float32 = 23.0 / 15
	LeadingLabel       float32 = 16.0 / 12
	LeadingQuestion    float32 = 27.0 / 22
	LeadingOption      float32 = 19.0 / 14
	LeadingDescription float32 = 17.0 / 13
)

const (
	WeightText    Weight = 400
	WeightTitle   Weight = 600
	WeightControl Weight = 650
	WeightStrong  Weight = 700
)

const (
	Space1 Dp = 4
	Space2 Dp = 8
	Space3 Dp = 12
	Space4 Dp = 16
	Space5 Dp = 20
	Space6 Dp = 24
	Space7 Dp = 28
	Space8 Dp = 32
)

const (
	RadiusEdit     Dp = 4
	RadiusDecision Dp = 7
	RadiusTag      Dp = 8
	RadiusControl  Dp = 10
	RadiusCard     Dp = 12
	RadiusField    Dp = 14
)

const (
	BorderWidth   Dp = 1
	FocusWidth    Dp = 2
	FocusOffset   Dp = 2
	ControlHeight Dp = 36
	TargetMin     Dp = 24
)

const (
	FieldHeight           Dp = 60
	FieldActionHeight     Dp = 42
	TagHeight             Dp = 28
	TagMinWidth           Dp = 84
	DotSize               Dp = 6
	ThreadDotSize         Dp = 8
	CommandMinHeight      Dp = 112
	CommandMargin         Dp = 22
	CommandPaddingBlock   Dp = 18
	DirectoryMargin       Dp = 5
	ReasonMargin          Dp = 18
	DecisionHeight        Dp = 42
	DecisionPadding       Dp = 13
	PrimaryDecisionHeight Dp = 48
	QuietDecisionGap      Dp = 10
	QuestionMargin        Dp = 6
	OptionHeight          Dp = 54
	OptionPaddingBlock    Dp = 7
	OptionPaddingInline   Dp = 11
	OptionMarkColumn      Dp = 20
	OptionDescriptionRow  Dp = 17
	OptionGap             Dp = 9
	OptionMarkSize        Dp = 16
	SelectionBarWidth     Dp = 2
	OtherGap              Dp = 14
	QuestionActionWidth   Dp = 92
	QuestionActionHeight  Dp = 38
	MetaHeight            Dp = 26
	DiffLineHeight        Dp = 24
	DiffNumberWidth       Dp = 29
	DiffSignWidth         Dp = 17
	DiffPadding           Dp = 5
	TargetNoteWidth       Dp = 112
	ElapsedWidthChars        = 7
)

const (
	WindowDefaultWidth    Dp = 1280
	WindowDefaultHeight   Dp = 800
	WindowMinWidth        Dp = 320
	WindowMinHeight       Dp = 320
	SidebarMinWidth       Dp = 256
	SidebarMaxWidth       Dp = 304
	TranscriptMaxWidth    Dp = 896
	ShellStackWidth       Dp = 832
	ThreadListStackCap    Dp = 288
	StatusCompactWidth    Dp = 640
	StatusDropFolderWidth Dp = 480
	StatusDropQueueWidth  Dp = 416
	StatusDropEffortWidth Dp = 256
	StatusDropMarkWidth   Dp = 192
	FollowDistance        Dp = 32
	RequestScrollStep     Dp = 48
	ModalMaxWidth         Dp = 1120
	IdleDockClearance     Dp = 96
	TitleBasis            Dp = 256
	BandMinWidth          Dp = 128
)

const (
	MotionBreath         = 1800 * time.Millisecond
	MotionQuick          = 120 * time.Millisecond
	BreathFloor  float32 = 0.4
)
