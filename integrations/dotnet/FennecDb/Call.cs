using System.Collections;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace FennecDb;

/// <summary>
/// A call of a named statement: <c>call &lt;name&gt; $1</c>, its arguments the one parameter -- an object of
/// values by name -- and the claims after them, <c>as $2</c>, the claims the server's token makes the call with (a
/// scoped token's own are its token's, and it may not say them). It is made by <see cref="Query.Call"/> and
/// is not bound to a collection or a client; <see cref="FennecClient.CallAsync"/> runs one by name.
///
///     Query.Call("transfer", new { from = 1, to = 2, amount = 300 }).ToCall()
///     // ("call transfer $1", [{ from: 1, to: 2, amount: 300 }])
///
/// No arguments is a statement of none, <c>call stamp</c>; an empty object is still one argument,
/// <c>call stamp $1</c>. The values are parameters as a document's are -- a <see cref="DateTime"/> its ISO text, a
/// <c>float[]</c> its numbers -- and never the text: the name is the one thing of a call written into it, so it is
/// checked, and that check is the injection boundary. Immutable: what the arguments and claims held when the call
/// was made is what it renders, however the dictionaries handed in change after.
/// </summary>
public sealed class Call
{
    // A named statement's name: ASCII letters, digits and _, a letter first, at most 59 bytes -- the engine's rule,
    // a path segment with nothing to escape. \z, not $, which lets a trailing newline through.
    static readonly Regex Valid = new(@"^[A-Za-z][A-Za-z0-9_]{0,58}\z", RegexOptions.CultureInvariant);

    readonly object? _args;
    readonly object? _claims;

    /// <summary>A call of <paramref name="name"/> with <paramref name="args"/>, a dictionary or an object's public
    /// properties (null: none), and <paramref name="claims"/> the same. A name that is no statement's, or
    /// arguments or claims that are not an object of values by name -- a list, a text, a number -- throw
    /// <see cref="FennecQueryException"/> here, before anything is sent.</summary>
    public Call(string name, object? args = null, object? claims = null)
    {
        if (name is null || !Valid.IsMatch(name)) throw Builder.Refuse($"invalid statement name: {Builder.Quote(name)}");
        Name = name;
        _args = Given(args, "arguments");
        _claims = Given(claims, "claims");
    }

    /// <summary>The statement's name.</summary>
    public string Name { get; }

    /// <summary>The call's FennecQL and its parameters, not sent: <c>call stamp</c>, <c>call transfer $1</c>,
    /// <c>call mine $1 as $2</c>, the arguments' parameter first.</summary>
    public (string Text, IReadOnlyList<object?> Parameters) ToCall()
    {
        var bind = new Binder();
        var text = $"call {Name}";
        if (_args is not null) text += $" {bind.Bind(_args)}";
        if (_claims is not null) text += $" as {bind.Bind(_claims)}";
        return (text, bind.Params);
    }

    // An object of values by name, or none: a snapshot, normalised as a document's values are.
    static object? Given(object? v, string what)
    {
        switch (v)
        {
            case null:
            case JsonElement { ValueKind: JsonValueKind.Null or JsonValueKind.Undefined }:
                return null;
            case JsonElement { ValueKind: JsonValueKind.Object } e:
                return e.Clone();
            case JsonElement:
            case string:
            case IEnumerable and not IDictionary:
                break;
            default:
                // A number, a bool, a date is no object; a class or a record is its public properties.
                if (v is IDictionary || Type.GetTypeCode(v.GetType()) == TypeCode.Object)
                    return Builder.Normalize(
                        Builder.DocOf(v).ToDictionary(kv => kv.Key, kv => kv.Value));
                break;
        }
        throw Builder.Refuse($"a call's {what} are an object of values by name");
    }
}
