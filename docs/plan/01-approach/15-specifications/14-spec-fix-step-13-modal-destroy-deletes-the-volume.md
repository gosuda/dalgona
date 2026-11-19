#### Spec-fix step 13. Modal Destroy deletes the volume when the sandbox is already gone
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/sandbox-services.md` line 19 says: "It MUST then delete the named volume." Keep that sentence.

- Where: `internal/sandboxsvc/modal/service.go:Service.Destroy`.
- Change: When `s.api.FromID` or `box.Terminate` fails with an error for which `isModalGone` is true, do not return that error. Continue to `s.api.DeleteVolume(ctx, volume)`, then `s.images.Delete(volume)`, and return nil. Keep the `modal: attach %s` and `modal: terminate %s` errors for every other error, and keep `modal: delete volume %s: %w` for a failed delete.
- Failing-first test: `TestServiceDestroyDeletesVolumeWhenSandboxIsGone` in `internal/sandboxsvc/modal/service_test.go`. Input: fakeAPI whose FromID returns sdk.NotFoundError for "sandbox-1" (the type isModalGone checks in service.go:19-22), then Destroy(ctx, "sandbox-1/dalgona-test") Expected: Destroy returns nil and api.deleted equals []string{"dalgona-test"}; today it returns an error and api.deleted is empty, so the test fails first The expected value comes from: docs/spec/sandbox-services.md:19 `It MUST then delete the named volume.`
- Evidence at commit `3c4b6f94`: internal/sandboxsvc/modal/service.go:159-162 and 165-168 return `sandboxsvc.ErrGone` before the volume delete at service.go:170-172; internal/remote/lifecycle.go:141-145 `if sandboxsvc.IsGone(err) { return b.appendLedger("destroyed", ...) }`, so the box is recorded as destroyed while its volume stays; service_test.go:64-70 fake FromID returns a plain error, so the gone path is untested.
- Proof: the test fails before the change and passes after it, and `just verify` passes.

