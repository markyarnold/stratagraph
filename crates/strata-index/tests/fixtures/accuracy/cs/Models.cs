// C# accuracy corpus — the model types. Two types each declare a `Save` method
// (the deliberate ambiguity: an unknown-receiver `Save()` cannot be
// disambiguated), and `Build` is declared exactly once repo-wide (the unique
// cross-file name a bare `Build()` resolves to at Inferred).
namespace App.Models
{
    public class User
    {
        public void Save() { }
    }

    public class Account
    {
        public void Save() { }
    }

    public static class Factory
    {
        // The single repo-wide `Build` — a bare cross-file `Build()` resolves here
        // (unique name → Inferred 0.80).
        public static int Build() { return 0; }
    }
}
