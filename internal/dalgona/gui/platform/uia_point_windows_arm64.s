#include "textflag.h"

TEXT ·uiaElementFromPointThunk(SB), NOSPLIT|NOFRAME, $0-0
	MOVD R1, R3
	FMOVD F0, R1
	FMOVD F1, R2
	MOVD $·uiaElementFromPointCallback(SB), R16
	MOVD (R16), R16
	JMP (R16)

TEXT ·uiaRangeFromPointThunk(SB), NOSPLIT|NOFRAME, $0-0
	MOVD R1, R3
	FMOVD F0, R1
	FMOVD F1, R2
	MOVD $·uiaRangeFromPointCallback(SB), R16
	MOVD (R16), R16
	JMP (R16)

TEXT ·uiaPointThunks(SB), NOSPLIT, $0-16
	MOVD $·uiaElementFromPointThunk(SB), R0
	MOVD R0, element+0(FP)
	MOVD $·uiaRangeFromPointThunk(SB), R0
	MOVD R0, rangeFrom+8(FP)
	RET
