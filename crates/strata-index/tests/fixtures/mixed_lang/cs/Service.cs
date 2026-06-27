// A C# source in the mixed-language fixture: a namespaced class whose method
// calls a same-file method (Extracted 0.95) and a this-method (Inferred 0.80),
// linked entirely within the C# (`cs`-tagged) resolution world — no
// cross-language edge to the TS or Python planes this slice.
namespace Mixed.Services
{
    public class CsService
    {
        public int Run()
        {
            // Same-file bare call → Extracted 0.95.
            var v = CsHelper();
            // this-receiver call to an own-type method → Inferred 0.80.
            return this.Compute(v);
        }

        private int CsHelper()
        {
            return 7;
        }

        private int Compute(int x)
        {
            return x + 1;
        }
    }
}
