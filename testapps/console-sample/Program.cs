// A non-ASP.NET workload, for exercising countercow's generic dashboard.
//
// Its job is to make the runtime counters move in a recognisable pattern rather than to compute
// anything: allocation comes in waves so the heap graph has a visible shape, a slice of objects
// is retained so gen 2 grows, and exceptions and lock contention tick along underneath.
//
// It must NOT reference any ASP.NET assembly — countercow decides which dashboard to show by
// whether ASP.NET providers report, and this is the negative case.

using System.Diagnostics;

var duration = args.Length > 0 && int.TryParse(args[0], out var seconds)
    ? TimeSpan.FromSeconds(seconds)
    : TimeSpan.FromMinutes(10);

Console.WriteLine($"countercow sample console — running for {duration}");
Console.WriteLine($"pid {Environment.ProcessId}");

var retained = new List<byte[]>();
var contended = new object();
var random = new Random(42);
var stopwatch = Stopwatch.StartNew();
var cycle = 0;

// A couple of threads fighting over one lock, so monitor-lock-contention-count is non-zero.
using var cancellation = new CancellationTokenSource(duration);
for (var i = 0; i < 2; i++)
{
    _ = Task.Run(() =>
    {
        while (!cancellation.Token.IsCancellationRequested)
        {
            lock (contended)
            {
                Thread.SpinWait(5_000);
            }
        }
    });
}

while (stopwatch.Elapsed < duration)
{
    cycle++;

    // Allocation in waves: the heap graph gets a sawtooth rather than a flat line.
    var burst = 20 + random.Next(80);
    for (var i = 0; i < burst; i++)
    {
        var block = new byte[64 * 1024];
        block[0] = (byte)i;

        // Retain roughly one in twenty, so gen 2 grows steadily and fragmentation appears.
        if (i % 20 == 0)
        {
            retained.Add(block);
        }
    }

    // Release the oldest retentions occasionally, so the heap does not only ever grow.
    if (cycle % 25 == 0 && retained.Count > 200)
    {
        retained.RemoveRange(0, 100);
    }

    // Thrown and caught: exception-count moves without the process dying.
    if (cycle % 3 == 0)
    {
        try
        {
            throw new InvalidOperationException($"sample failure {cycle}");
        }
        catch (InvalidOperationException)
        {
            // Expected.
        }
    }

    // Threadpool work, so queue length and completed items move.
    if (cycle % 5 == 0)
    {
        for (var i = 0; i < 50; i++)
        {
            ThreadPool.QueueUserWorkItem(_ => Thread.SpinWait(1_000));
        }
    }

    Thread.Sleep(100);
}

Console.WriteLine($"done after {cycle} cycles, {retained.Count} blocks retained");
