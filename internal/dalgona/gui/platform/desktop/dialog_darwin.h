#include <stddef.h>
#include <stdint.h>

enum {
	DalgonaDialogOpen = 1,
	DalgonaDialogFolder = 2,
	DalgonaDialogSave = 3,
};

enum {
	DalgonaDialogChosen = 0,
	DalgonaDialogCancelled = 1,
	DalgonaDialogNoWindow = 2,
	DalgonaDialogRemote = 3,
};

uintptr_t dalgona_dialog_parent(uintptr_t view);
void dalgona_dialog_parent_release(uintptr_t parent);
uintptr_t dalgona_dialog_begin(uintptr_t parent, uint8_t kind, const uint8_t *title, size_t titleLength, const uint8_t *name, size_t nameLength, const uint8_t *extensions, size_t extensionsLength);
int dalgona_dialog_wait(uintptr_t dialog, const uint8_t **paths, size_t *length);
void dalgona_dialog_cancel(uintptr_t dialog);
void dalgona_dialog_release(uintptr_t dialog);
