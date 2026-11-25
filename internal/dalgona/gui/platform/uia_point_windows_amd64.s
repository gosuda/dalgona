#include "textflag.h"

TEXT ·uiaElementFromPointThunk(SB), NOSPLIT|NOFRAME, $0-0
	MOVQ X1, DX
	MOVQ X2, R8
	MOVQ ·uiaElementFromPointCallback(SB), AX
	JMP AX

TEXT ·uiaRangeFromPointThunk(SB), NOSPLIT|NOFRAME, $0-0
	MOVQ R8, R9
	MOVQ 8(DX), R8
	MOVQ 0(DX), DX
	MOVQ ·uiaRangeFromPointCallback(SB), AX
	JMP AX

TEXT ·uiaPointThunks(SB), NOSPLIT, $0-16
	MOVQ $·uiaElementFromPointThunk(SB), AX
	MOVQ AX, element+0(FP)
	MOVQ $·uiaRangeFromPointThunk(SB), AX
	MOVQ AX, rangeFrom+8(FP)
	RET
