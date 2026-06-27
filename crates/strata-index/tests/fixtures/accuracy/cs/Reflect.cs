// C# accuracy corpus — the reflection honesty case. `GetMethod`/`Invoke` are
// reflective dispatch on unknown receivers, and NO method named `GetMethod` or
// `Invoke` is defined anywhere in this corpus, so each resolves to NOTHING (no
// edge, surfaced as unresolved). Crucially the reflected `"Run"` string is never
// invented as a call — there is no `Run` here to (wrongly) link to anyway.
namespace App
{
    public class Reflector
    {
        public void Reflect(System.Type t)
        {
            var mi = t.GetMethod("Run"); // reflective lookup → unresolved
            mi.Invoke(this, null);       // reflective dispatch → unresolved
        }
    }
}
