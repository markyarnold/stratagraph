// C# accuracy corpus — the service exercising every linking rule from one method.
namespace App
{
    public class Service
    {
        public void Run(App.Models.Account acct)
        {
            Helper();        // same-file def         → Extracted 0.95
            this.Helper();   // own-type method        → Inferred 0.80
            Build();         // unique cross-file name → Inferred 0.80
            acct.Save();     // unknown receiver, 2x   → Ambiguous 0.35 (fan-out)
            Ghost();         // unknown name           → no edge (unresolved)
        }

        private void Helper() { }
    }
}
