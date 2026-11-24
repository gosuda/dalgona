package figure

import (
	"os"
	"testing"
)

func TestChildEnvironmentNeverInherits(t *testing.T) {
	for _, name := range []string{"SYSTEMROOT", "SystemRoot"} {
		value, present := os.LookupEnv(name)
		if err := os.Unsetenv(name); err != nil {
			t.Fatal(err)
		}
		if present {
			t.Cleanup(func() { _ = os.Setenv(name, value) })
		} else {
			t.Cleanup(func() { _ = os.Unsetenv(name) })
		}
	}
	env := childEnvironment()
	if env == nil {
		t.Fatal("child environment is nil, so the renderer would inherit the GUI process environment")
	}
	if len(env) != 0 {
		t.Fatalf("child environment = %q, want no entries without platform variables", env)
	}
	t.Setenv("SystemRoot", `C:\Windows`)
	env = childEnvironment()
	if len(env) != 1 || env[0] != `SystemRoot=C:\Windows` {
		t.Fatalf("child environment = %q, want only the platform variable", env)
	}
}
