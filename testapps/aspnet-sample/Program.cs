// A deliberately small ASP.NET Core app for exercising countercow's ASP.NET dashboard.
//
// The endpoints exist to move specific counters: /alloc drives allocation rate and heap growth,
// /leak grows gen 2 so fragmentation and collections become visible, and /throw drives the
// exception counter. Hitting / alone is enough to move requests-per-second and Kestrel
// connection counts.

using System.Diagnostics;

var builder = WebApplication.CreateBuilder(args);
var app = builder.Build();

// Retained across requests so the heap actually grows rather than being collected immediately.
var retained = new List<byte[]>();

app.MapGet("/", () => new
{
    service = "countercow sample api",
    uptime = Environment.TickCount64 / 1000,
    endpoints = new[] { "/alloc", "/leak", "/throw", "/slow", "/work" },
});

// Short-lived allocations: allocation rate climbs, gen 0 collections follow, nothing is retained.
app.MapGet("/alloc", (int mb = 8) =>
{
    long allocated = 0;
    for (var i = 0; i < mb; i++)
    {
        var block = new byte[1024 * 1024];
        block[0] = 1;
        allocated += block.Length;
    }
    return Results.Ok(new { allocatedBytes = allocated });
});

// Retained allocations: working set and gen 2 grow and stay grown.
app.MapGet("/leak", (int mb = 4) =>
{
    for (var i = 0; i < mb; i++)
    {
        retained.Add(new byte[1024 * 1024]);
    }
    return Results.Ok(new { retainedBlocks = retained.Count });
});

// Thrown and caught, so the process stays up while exception-count moves.
app.MapGet("/throw", (int count = 10) =>
{
    var caught = 0;
    for (var i = 0; i < count; i++)
    {
        try
        {
            throw new InvalidOperationException("sample failure");
        }
        catch (InvalidOperationException)
        {
            caught++;
        }
    }
    return Results.Ok(new { caught });
});

// Holds a request open, so current-requests and Kestrel connection counts are observable.
app.MapGet("/slow", async (CancellationToken token, int ms = 2000) =>
{
    await Task.Delay(ms, token);
    return Results.Ok(new { sleptMs = ms });
});

// Queues work items, so the threadpool counters move.
app.MapGet("/work", (int items = 200) =>
{
    var started = Stopwatch.GetTimestamp();
    for (var i = 0; i < items; i++)
    {
        ThreadPool.QueueUserWorkItem(_ => Thread.SpinWait(10_000));
    }
    return Results.Ok(new { queued = items, atTicks = started });
});

// An unhandled 500, so failed-requests moves.
app.MapGet("/fail", () => Results.Problem("deliberate failure"));

app.Run();
