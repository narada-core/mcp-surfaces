using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Narada.Filesystem.Dotnet;

internal static class Program
{
    private static readonly JsonSerializerOptions JsonOptions = new() { WriteIndented = false };
    private static readonly List<string> AllowedRoots = new();
    private static string Mode = "read";
    private static string? OutputRoot;

    public static int Main(string[] args)
    {
        ParseArguments(args);
        if (AllowedRoots.Count == 0)
        {
            AllowedRoots.Add(CanonicalPath(Environment.CurrentDirectory));
        }

        while (Console.ReadLine() is { } line)
        {
            if (string.IsNullOrWhiteSpace(line))
            {
                continue;
            }

            try
            {
                using var document = JsonDocument.Parse(line);
                var request = document.RootElement;
                if (!request.TryGetProperty("id", out var idElement))
                {
                    continue;
                }

                var id = JsonNode.Parse(idElement.GetRawText());
                var method = request.TryGetProperty("method", out var methodElement)
                    ? methodElement.GetString() ?? string.Empty
                    : string.Empty;
                var parameters = request.TryGetProperty("params", out var paramsElement)
                    ? paramsElement
                    : default;
                var response = HandleRequest(id, method, parameters);
                Console.WriteLine(response.ToJsonString(JsonOptions));
                Console.Out.Flush();
            }
            catch (Exception error)
            {
                var response = ErrorResponse(null, -32700, "invalid_request", error.Message);
                Console.WriteLine(response.ToJsonString(JsonOptions));
                Console.Out.Flush();
            }
        }

        return 0;
    }

    private static JsonObject HandleRequest(JsonNode? id, string method, JsonElement parameters)
    {
        return method switch
        {
            "initialize" => SuccessResponse(id, InitializeResult()),
            "tools/list" => SuccessResponse(id, new JsonObject { ["tools"] = ToolDefinitions() }),
            "tools/call" => HandleToolCall(id, parameters),
            "resources/list" => SuccessResponse(id, new JsonObject { ["resources"] = new JsonArray() }),
            "prompts/list" => SuccessResponse(id, new JsonObject { ["prompts"] = new JsonArray() }),
            "completion/complete" => SuccessResponse(id, new JsonObject { ["completion"] = new JsonObject { ["values"] = new JsonArray(), ["hasMore"] = false } }),
            "logging/setLevel" => SuccessResponse(id, new JsonObject()),
            _ => ErrorResponse(id, -32601, "method_not_found", "Unknown MCP method: " + method),
        };
    }

    private static JsonObject HandleToolCall(JsonNode? id, JsonElement parameters)
    {
        var name = StringField(parameters, "name") ?? string.Empty;
        var arguments = ObjectField(parameters, "arguments");
        try
        {
            var value = DispatchTool(name, arguments);
            return SuccessResponse(id, ToolResult(value, false));
        }
        catch (FilesystemException error)
        {
            var diagnostic = ErrorDiagnostic(error.Code, error.Message, name);
            return SuccessResponse(id, ToolResult(diagnostic, true));
        }
        catch (Exception error)
        {
            var diagnostic = ErrorDiagnostic("native_dotnet_filesystem_error", error.Message, name);
            return SuccessResponse(id, ToolResult(diagnostic, true));
        }
    }

    private static JsonObject DispatchTool(string name, JsonElement args)
    {
        return name switch
        {
            "fs_guidance" => Guidance(),
            "fs_doctor" => Doctor(),
            "fs_read_file" => ReadFile(args),
            "fs_read_file_range" => ReadFileRange(args),
            "fs_stat" => Stat(args),
            "fs_glob_search" => GlobSearch(args),
            "fs_grep_search" => GrepSearch(args),
            "fs_file_metrics" => FileMetrics(args),
            "fs_repository_inventory" => RepositoryInventory(args),
            "fs_patch_outcome_show" => PatchOutcome(args),
            _ => throw new FilesystemException("unknown_tool", "Unknown filesystem tool: " + name),
        };
    }

    private static JsonObject InitializeResult()
    {
        return new JsonObject
        {
            ["protocolVersion"] = "2024-11-05",
            ["capabilities"] = new JsonObject
            {
                ["tools"] = new JsonObject(),
                ["resources"] = new JsonObject(),
                ["prompts"] = new JsonObject(),
            },
            ["serverInfo"] = new JsonObject
            {
                ["name"] = "local-filesystem-dotnet-native",
                ["version"] = "0.1.0",
            },
        };
    }

    private static JsonArray ToolDefinitions()
    {
        var definitions = new JsonArray();
        AddTool(definitions, "fs_guidance", "Guidance for safe local filesystem operations.");
        AddTool(definitions, "fs_read_file", "Read a bounded line window from a file.");
        AddTool(definitions, "fs_read_file_range", "Read a precise bounded line range.");
        AddTool(definitions, "fs_stat", "Inspect a file or directory.");
        AddTool(definitions, "fs_glob_search", "Find files under an allowed directory.");
        AddTool(definitions, "fs_grep_search", "Search file contents under an allowed directory.");
        AddTool(definitions, "fs_file_metrics", "Return bounded file metadata and line counts.");
        AddTool(definitions, "fs_doctor", "Inspect filesystem policy posture.");
        AddTool(definitions, "fs_repository_inventory", "Return a bounded repository inventory.");
        AddTool(definitions, "fs_patch_outcome_show", "Read a persisted patch outcome.");
        return definitions;
    }

    private static void AddTool(JsonArray definitions, string name, string description)
    {
        definitions.Add((JsonNode)new JsonObject
        {
            ["name"] = name,
            ["description"] = description,
            ["inputSchema"] = new JsonObject
            {
                ["type"] = "object",
                ["additionalProperties"] = true,
                ["properties"] = new JsonObject(),
            },
        });
    }

    private static JsonObject Guidance()
    {
        return new JsonObject
        {
            ["schema"] = "local.filesystem.guidance.v1",
            ["status"] = "ok",
            ["surface_id"] = "local-filesystem",
            ["mode"] = Mode,
            ["principles"] = new JsonArray(
                "Use fs_doctor before relying on relative paths.",
                "Prefer bounded search and range reads.",
                "Treat structuredContent as authoritative.",
                "This NativeAOT lane is read-only."
            ),
            ["tools"] = new JsonObject
            {
                ["discovery"] = "fs_glob_search or fs_grep_search",
                ["bounded_read"] = "fs_read_file_range",
                ["metadata"] = "fs_stat or fs_file_metrics",
            },
        };
    }

    private static JsonObject Doctor()
    {
        var roots = new JsonArray();
        foreach (var root in AllowedRoots)
        {
            roots.Add((JsonNode)new JsonObject
            {
                ["path"] = root,
                ["canonical_path"] = root,
                ["source"] = "cli",
                ["exists"] = Directory.Exists(root),
            });
        }

        var result = new JsonObject
        {
            ["schema"] = "local.filesystem.doctor.v1",
            ["status"] = "ok",
            ["surface_id"] = "local-filesystem",
            ["mode"] = Mode,
            ["allowed_roots"] = roots,
            ["relative_path_resolution"] = new JsonObject
            {
                ["base"] = AllowedRoots[0],
                ["rule"] = "relative paths resolve against the first allowed root",
            },
            ["client_roots"] = new JsonObject
            {
                ["support"] = "unsupported",
                ["roots"] = new JsonArray(),
            },
            ["capabilities"] = new JsonObject
            {
                ["read"] = true,
                ["write"] = false,
                ["native_aot"] = true,
            },
        };
        if (OutputRoot is not null)
        {
            result["output_root"] = OutputRoot;
        }
        return result;
    }

    private static JsonObject ReadFile(JsonElement args)
    {
        var offset = Math.Max(1, IntField(args, "offset") ?? IntField(args, "start_line") ?? 1);
        var limit = Math.Max(1, IntField(args, "limit") ?? 400);
        if (limit > 1000)
        {
            throw new FilesystemException("fs_read_file_limit_exceeds_max", "fs_read_file limit exceeds the maximum of 1000 lines; paginate the request");
        }
        return ReadWindow(args, offset, limit, "fs_read_file");
    }

    private static JsonObject ReadFileRange(JsonElement args)
    {
        var start = IntField(args, "start_line");
        var end = IntField(args, "end_line");
        if (start is null || start < 1)
        {
            throw new FilesystemException("start_line_must_be_positive_integer", "start_line must be a positive integer");
        }
        if (end is null || end < start)
        {
            throw new FilesystemException("end_line_must_be_greater_than_or_equal_start_line", "end_line must be greater than or equal to start_line");
        }
        var span = Math.Max(1L, (long)end.Value - start.Value + 1);
        if (span > 1000)
        {
            throw new FilesystemException("fs_read_file_range_limit_exceeds_max", "fs_read_file_range span exceeds the maximum of 1000 lines; paginate the request");
        }
        return ReadWindow(args, start.Value, (int)span, "fs_read_file_range");
    }

    private static JsonObject ReadWindow(JsonElement args, int startLine, int limit, string operation)
    {
        var path = ResolvePath(StringField(args, "path"), operation);
        if (!File.Exists(path))
        {
            throw new FilesystemException("path_not_found", "File does not exist: " + path);
        }

        var allLines = File.ReadLines(path).ToList();
        var lines = new JsonArray();
        var startIndex = startLine - 1;
        for (var index = startIndex; index < Math.Min(allLines.Count, startIndex + limit); index++)
        {
            lines.Add((JsonNode)new JsonObject
            {
                ["line_number"] = index + 1,
                ["text"] = allLines[index],
            });
        }
        var complete = startIndex + limit >= allLines.Count;

        var result = new JsonObject
        {
            ["schema"] = "local.filesystem.read.v1",
            ["status"] = "ok",
            ["operation"] = operation,
            ["path"] = path,
            ["start_line"] = startLine,
            ["requested_limit"] = limit,
            ["lines"] = lines,
            ["returned_lines"] = lines.Count,
            ["line_window_complete"] = complete,
            ["total_lines_exact"] = true,
            ["total_lines_status"] = "exact",
            ["content_sha256"] = Sha256File(path),
            ["content_hash_scope"] = "full_file",
            ["hash_source"] = "live_file_bytes",
            ["cache_used"] = false,
            ["max_limit"] = 1000,
            ["limit_adjusted"] = false,
            ["pagination_required"] = !complete,
            ["total_lines"] = allLines.Count,
        };
        return result;
    }

    private static JsonObject Stat(JsonElement args)
    {
        var path = ResolvePath(StringField(args, "path"), "fs_stat");
        if (File.Exists(path))
        {
            var info = new FileInfo(path);
            return new JsonObject
            {
                ["schema"] = "local.filesystem.stat.v1",
                ["status"] = "ok",
                ["type"] = "file",
                ["path"] = path,
                ["size"] = info.Length,
                ["mtime"] = info.LastWriteTimeUtc.ToString("O"),
                ["sha256"] = Sha256File(path),
            };
        }

        if (!Directory.Exists(path))
        {
            throw new FilesystemException("path_not_found", "Path does not exist: " + path);
        }

        var entries = EnumerateFiles(path);
        var digestInput = string.Join("\n", entries.Select(item =>
        {
            var info = new FileInfo(item);
            return RelativePath(path, item) + "|" + info.Length + "|" + info.LastWriteTimeUtc.Ticks;
        }));
        return new JsonObject
        {
            ["schema"] = "local.filesystem.stat.v1",
            ["status"] = "ok",
            ["type"] = "directory",
            ["path"] = path,
            ["entry_count"] = entries.Count,
            ["tree_entry_count"] = entries.Count,
            ["tree_truncated"] = false,
            ["tree_sha256"] = Sha256Text(digestInput),
            ["mtime"] = Directory.GetLastWriteTimeUtc(path).ToString("O"),
        };
    }

    private static JsonObject GlobSearch(JsonElement args)
    {
        var directory = ResolvePath(StringField(args, "directory") ?? StringField(args, "path"), "fs_glob_search");
        var pattern = StringField(args, "pattern") ?? "**/*";
        var limit = Math.Max(1, IntField(args, "limit") ?? 100);
        var offset = Math.Max(0, IntField(args, "offset") ?? 0);
        var matches = EnumerateFiles(directory)
            .Where(path => MatchesGlob(path, pattern))
            .Where(path => !Ignored(path, args))
            .Select(path => RelativePath(directory, path))
            .ToList();
        return SearchPage(
            "local.filesystem.glob.v1",
            matches.Select(path => (JsonNode?)JsonValue.Create(path)),
            offset,
            limit,
            directory,
            pattern);
    }

    private static JsonObject GrepSearch(JsonElement args)
    {
        var directory = ResolvePath(StringField(args, "path") ?? StringField(args, "directory"), "fs_grep_search");
        var pattern = StringField(args, "pattern") ?? throw new FilesystemException("missing_pattern", "pattern is required");
        var outputMode = StringField(args, "output_mode") ?? "files_with_matches";
        var limit = Math.Max(1, IntField(args, "limit") ?? 100);
        var offset = Math.Max(0, IntField(args, "offset") ?? 0);
        var rows = new List<JsonObject>();

        foreach (var file in EnumerateFiles(directory).Where(path => !Ignored(path, args)))
        {
            var fileMatches = new List<JsonObject>();
            var lineNumber = 0;
            foreach (var line in File.ReadLines(file))
            {
                if (line.Contains(pattern, StringComparison.Ordinal))
                {
                    fileMatches.Add(new JsonObject
                    {
                        ["path"] = RelativePath(directory, file),
                        ["line_number"] = lineNumber,
                        ["line"] = line,
                        ["text"] = line,
                    });
                }
                lineNumber++;
            }

            if (fileMatches.Count == 0)
            {
                continue;
            }

            if (outputMode == "content")
            {
                rows.AddRange(fileMatches);
            }
            else if (outputMode == "count_matches")
            {
                rows.Add(new JsonObject
                {
                    ["path"] = RelativePath(directory, file),
                    ["count"] = fileMatches.Count,
                });
            }
            else
            {
                rows.Add(new JsonObject { ["path"] = RelativePath(directory, file) });
            }
        }

        return SearchPage("local.filesystem.grep.v1", rows.Cast<JsonNode?>(), offset, limit, directory, pattern, outputMode);
    }

    private static JsonObject FileMetrics(JsonElement args)
    {
        var directory = ResolvePath(StringField(args, "directory") ?? StringField(args, "root"), "fs_file_metrics");
        var pattern = StringField(args, "pattern") ?? "**/*";
        var limit = Math.Max(1, IntField(args, "limit") ?? 100);
        var offset = Math.Max(0, IntField(args, "offset") ?? 0);
        var rows = new List<JsonObject>();
        foreach (var file in EnumerateFiles(directory).Where(path => MatchesGlob(path, pattern)))
        {
            var info = new FileInfo(file);
            var lineCount = File.ReadLines(file).Count();
            rows.Add(new JsonObject
            {
                ["path"] = RelativePath(directory, file),
                ["size"] = info.Length,
                ["byte_size"] = info.Length,
                ["line_count"] = lineCount,
                ["line_count_status"] = "exact",
                ["type"] = "file",
            });
        }

        var result = SearchPage("local.filesystem.file_metrics.v1", rows.Cast<JsonNode?>(), offset, limit, directory, pattern);
        result["total_bytes"] = rows.Sum(row => row["byte_size"]?.GetValue<long>() ?? 0);
        return result;
    }

    private static JsonObject RepositoryInventory(JsonElement args)
    {
        var path = ResolvePath(StringField(args, "path"), "fs_repository_inventory");
        var files = EnumerateFiles(path).Take(500).Select(item => RelativePath(path, item)).ToArray();
        return new JsonObject
        {
            ["schema"] = "local.filesystem.repository_inventory.v1",
            ["status"] = "ok",
            ["path"] = path,
            ["files"] = new JsonArray(files.Select(path => (JsonNode?)JsonValue.Create(path)).ToArray()),
            ["returned"] = files.Length,
            ["truncated"] = EnumerateFiles(path).Count() > files.Length,
        };
    }

    private static JsonObject PatchOutcome(JsonElement args)
    {
        var operationId = StringField(args, "operation_id") ?? string.Empty;
        return new JsonObject
        {
            ["schema"] = "local.filesystem.apply_patch.outcome.v1",
            ["status"] = "not_found",
            ["operation_id"] = operationId,
            ["message"] = "The NativeAOT read-only applet does not apply patches.",
        };
    }

    private static JsonObject SearchPage(string schema, IEnumerable<JsonNode?> source, int offset, int limit, string directory, string pattern, string? outputMode = null)
    {
        var all = source.ToList();
        var page = all.Skip(offset).Take(limit).ToList();
        var matches = new JsonArray(page.ToArray());
        var result = new JsonObject
        {
            ["schema"] = schema,
            ["status"] = "ok",
            ["directory"] = directory,
            ["pattern"] = pattern,
            ["matches"] = matches,
            ["match_objects"] = matches.DeepClone(),
            ["match_objects_authoritative"] = true,
            ["count"] = all.Count,
            ["count_exact"] = true,
            ["returned"] = page.Count,
            ["has_more"] = offset + page.Count < all.Count,
        };
        if (offset + page.Count < all.Count)
        {
            result["next_offset"] = offset + page.Count;
        }
        if (outputMode is not null)
        {
            result["output_mode"] = outputMode;
        }
        return result;
    }

    private static List<string> EnumerateFiles(string directory)
    {
        try
        {
            return Directory.EnumerateFiles(directory, "*", SearchOption.AllDirectories)
                .OrderBy(path => path, StringComparer.OrdinalIgnoreCase)
                .ToList();
        }
        catch (Exception error)
        {
            throw new FilesystemException("search_failed", error.Message);
        }
    }

    private static bool MatchesGlob(string path, string pattern)
    {
        var fileName = Path.GetFileName(path);
        if (pattern is "**/*" or "*" or "**")
        {
            return true;
        }
        if (pattern.StartsWith("**/*.", StringComparison.Ordinal))
        {
            return fileName.EndsWith(pattern[4..], StringComparison.OrdinalIgnoreCase);
        }
        if (pattern.StartsWith("*.", StringComparison.Ordinal))
        {
            return fileName.EndsWith(pattern[1..], StringComparison.OrdinalIgnoreCase);
        }
        return string.Equals(fileName, pattern, StringComparison.OrdinalIgnoreCase);
    }

    private static bool Ignored(string path, JsonElement args)
    {
        if (!args.TryGetProperty("ignore", out var ignores) || ignores.ValueKind != JsonValueKind.Array)
        {
            return false;
        }
        foreach (var ignore in ignores.EnumerateArray())
        {
            var value = ignore.GetString();
            if (!string.IsNullOrWhiteSpace(value) && path.Contains(value, StringComparison.OrdinalIgnoreCase))
            {
                return true;
            }
        }
        return false;
    }

    private static string ResolvePath(string? raw, string operation)
    {
        var candidate = string.IsNullOrWhiteSpace(raw) ? "." : raw;
        var path = Path.IsPathRooted(candidate)
            ? CanonicalPath(candidate)
            : CanonicalPath(Path.Combine(AllowedRoots[0], candidate));
        if (!AllowedRoots.Any(root => IsWithin(root, path)))
        {
            throw new FilesystemException("path_outside_allowed_roots", operation + ": path is outside allowed roots");
        }
        return path;
    }

    private static bool IsWithin(string root, string path)
    {
        var normalizedRoot = root.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar) + Path.DirectorySeparatorChar;
        return string.Equals(path, root, StringComparison.OrdinalIgnoreCase)
            || path.StartsWith(normalizedRoot, StringComparison.OrdinalIgnoreCase);
    }

    private static string CanonicalPath(string path) => Path.GetFullPath(path);

    private static string RelativePath(string root, string path) =>
        Path.GetRelativePath(root, path).Replace(Path.DirectorySeparatorChar, '/');

    private static string Sha256File(string path)
    {
        using var stream = File.OpenRead(path);
        return Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();
    }

    private static string Sha256Text(string value) =>
        Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(value))).ToLowerInvariant();

    private static JsonObject ToolResult(JsonObject value, bool isError)
    {
        var result = new JsonObject
        {
            ["content"] = new JsonArray(new JsonObject
            {
                ["type"] = "text",
                ["text"] = value.ToJsonString(JsonOptions),
            }),
            ["structuredContent"] = value,
        };
        if (isError)
        {
            result["isError"] = true;
        }
        return result;
    }

    private static JsonObject ErrorDiagnostic(string code, string message, string operation)
    {
        return new JsonObject
        {
            ["schema"] = "local.filesystem.error.v1",
            ["status"] = "error",
            ["error_code"] = code,
            ["message"] = message,
            ["details"] = new JsonObject
            {
                ["operation"] = operation,
            },
        };
    }

    private static JsonObject SuccessResponse(JsonNode? id, JsonObject result) =>
        new() { ["jsonrpc"] = "2.0", ["id"] = id, ["result"] = result };

    private static JsonObject ErrorResponse(JsonNode? id, int code, string errorCode, string message) =>
        new()
        {
            ["jsonrpc"] = "2.0",
            ["id"] = id,
            ["error"] = new JsonObject
            {
                ["code"] = code,
                ["message"] = message,
                ["data"] = new JsonObject { ["error_code"] = errorCode },
            },
        };

    private static string? StringField(JsonElement value, string name)
    {
        return value.ValueKind == JsonValueKind.Object
            && value.TryGetProperty(name, out var element)
            && element.ValueKind == JsonValueKind.String
            ? element.GetString()
            : null;
    }

    private static JsonElement ObjectField(JsonElement value, string name)
    {
        return value.ValueKind == JsonValueKind.Object
            && value.TryGetProperty(name, out var element)
            && element.ValueKind == JsonValueKind.Object
            ? element
            : default;
    }

    private static int? IntField(JsonElement value, string name)
    {
        if (value.ValueKind != JsonValueKind.Object || !value.TryGetProperty(name, out var element))
        {
            return null;
        }
        return element.ValueKind == JsonValueKind.Number && element.TryGetInt32(out var number) ? number : null;
    }

    private static void ParseArguments(string[] args)
    {
        for (var index = 0; index < args.Length; index++)
        {
            var argument = args[index];
            if (argument == "filesystem")
            {
                continue;
            }
            if (argument == "--mode" && index + 1 < args.Length)
            {
                Mode = args[++index];
            }
            else if (argument == "--allowed-root" && index + 1 < args.Length)
            {
                AllowedRoots.Add(CanonicalPath(args[++index]));
            }
            else if (argument == "--anchored-allowed-root" && index + 1 < args.Length)
            {
                var value = args[++index];
                var separator = value.IndexOf(':');
                if (separator > 0 && value[..separator].Equals("user_home", StringComparison.OrdinalIgnoreCase))
                {
                    AllowedRoots.Add(CanonicalPath(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), value[(separator + 1)..])));
                }
            }
            else if (argument is "--output-root" or "--audit-log" or "--roots-config" or "--trust-config")
            {
                if (index + 1 < args.Length)
                {
                    var value = args[++index];
                    if (argument == "--output-root")
                    {
                        OutputRoot = CanonicalPath(value);
                    }
                }
            }
        }

        if (!Mode.Equals("read", StringComparison.OrdinalIgnoreCase))
        {
            Mode = "read";
        }
    }

    private sealed class FilesystemException(string code, string message) : Exception(message)
    {
        public string Code { get; } = code;
    }
}
