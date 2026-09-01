import type { McpToolDefinition } from '@narada-core/mcp-fabric-contracts';

// Generated from the native tools/list registry. Do not hand-edit.
const TOOLS = {
  "read": [
  {
    "name": "fs_guidance",
    "canonical_name": "fs_guidance",
    "description": "Show model-facing operating guidance for local-filesystem MCP workflows.",
    "inputSchema": {
      "title": "fs_guidance arguments",
      "type": "object",
      "properties": {
        "workflow": {
          "type": "string",
          "maxLength": 32768
        },
        "tool": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_guidance",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_guidance result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_read_file",
    "canonical_name": "fs_read_file",
    "description": "Read a text file under an allowed root with line offset and limit.",
    "inputSchema": {
      "title": "fs_read_file arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "offset": {
          "type": "integer",
          "minimum": 1,
          "maximum": 10000000,
          "default": 1,
          "description": "One-based first line to return."
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 1000,
          "default": 400,
          "description": "Maximum lines returned; paginate requests over 1,000 lines."
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 60000,
          "default": 5000
        }
      },
      "required": [
        "path"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_read_file",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_read_file result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_read_file_range",
    "canonical_name": "fs_read_file_range",
    "description": "Read a logical text-file line range under an allowed root. Lines are 1-based and inclusive; ranges over 1,000 lines return a bounded page with continuation.arguments for the same MCP tool.",
    "inputSchema": {
      "title": "fs_read_file_range arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "start_line": {
          "type": "integer",
          "minimum": 1,
          "maximum": 10000000,
          "description": "Inclusive logical start line."
        },
        "end_line": {
          "type": "integer",
          "minimum": 1,
          "maximum": 10000000,
          "description": "Inclusive logical end line. Requests spanning over 1,000 lines return a bounded page; follow continuation.arguments until has_more is false."
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 60000,
          "default": 5000
        }
      },
      "required": [
        "path",
        "start_line",
        "end_line"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_read_file_range",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_read_file_range result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_stat",
    "canonical_name": "fs_stat",
    "description": "Return file or directory metadata under an allowed root.",
    "inputSchema": {
      "title": "fs_stat arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        }
      },
      "required": [
        "path"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_stat",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_stat result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_glob_search",
    "canonical_name": "fs_glob_search",
    "description": "List files under an allowed root using ripgrep file globbing. Empty matches return ok with count 0.",
    "inputSchema": {
      "title": "fs_glob_search arguments",
      "type": "object",
      "properties": {
        "pattern": {
          "type": "string",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "default": ".",
          "maxLength": 32768
        },
        "ignore": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 500,
          "default": 100
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        },
        "cache_policy": {
          "type": "string",
          "enum": [
            "auto",
            "snapshot",
            "refresh",
            "bypass"
          ],
          "default": "auto",
          "maxLength": 32768
        },
        "snapshot_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [
        "pattern"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_glob_search",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_glob_search result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_repository_inventory",
    "canonical_name": "fs_repository_inventory",
    "description": "Return a bounded candidate-source inventory under an allowed root, excluding generated runtime artifacts by default. Use git-mcp for authoritative tracked and ignored state.",
    "inputSchema": {
      "title": "fs_repository_inventory arguments",
      "type": "object",
      "properties": {
        "pattern": {
          "type": "string",
          "default": "**/*",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "description": "Canonical inventory scope; mutually exclusive with root.",
          "maxLength": 32768
        },
        "root": {
          "type": "string",
          "description": "Compatibility alias for directory; mutually exclusive with directory.",
          "maxLength": 32768
        },
        "ignore": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "include_generated": {
          "type": "boolean",
          "default": false
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 500,
          "default": 100
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        },
        "cache_policy": {
          "type": "string",
          "enum": [
            "auto",
            "snapshot",
            "refresh",
            "bypass"
          ],
          "default": "auto",
          "maxLength": 32768
        },
        "snapshot_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_repository_inventory",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_repository_inventory result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_file_metrics",
    "canonical_name": "fs_file_metrics",
    "description": "Return bounded metadata-only file metrics under an allowed root.",
    "inputSchema": {
      "title": "fs_file_metrics arguments",
      "type": "object",
      "properties": {
        "pattern": {
          "type": "string",
          "default": "**/*",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "default": ".",
          "maxLength": 32768
        },
        "root": {
          "type": "string",
          "maxLength": 32768
        },
        "ignore": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "exclude": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 100,
          "default": 100
        },
        "max_bytes_per_file": {
          "type": "integer",
          "minimum": 1,
          "maximum": 1073741824,
          "default": 8388608
        },
        "max_total_scan_bytes": {
          "type": "integer",
          "minimum": 1,
          "maximum": 1073741824,
          "default": 268435456
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        },
        "cache_policy": {
          "type": "string",
          "enum": [
            "auto",
            "snapshot",
            "refresh",
            "bypass"
          ],
          "default": "auto",
          "maxLength": 32768
        },
        "snapshot_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_file_metrics",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_file_metrics result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_search",
    "canonical_name": "fs_search",
    "description": "Find matching lines, matching files, or per-file counts under one allowed directory with enforced item and response budgets.",
    "inputSchema": {
      "title": "fs_search arguments",
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "default": ".",
          "maxLength": 32768
        },
        "syntax": {
          "type": "string",
          "enum": [
            "literal",
            "regex"
          ],
          "default": "literal",
          "maxLength": 32768
        },
        "result_kind": {
          "type": "string",
          "enum": [
            "matches",
            "files",
            "counts"
          ],
          "default": "matches",
          "maxLength": 32768
        },
        "file_glob": {
          "type": "string",
          "maxLength": 32768
        },
        "exclude": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "case": {
          "type": "string",
          "enum": [
            "smart",
            "sensitive",
            "insensitive"
          ],
          "default": "smart",
          "maxLength": 32768
        },
        "max_results": {
          "type": "integer",
          "minimum": 1,
          "maximum": 100,
          "default": 20
        },
        "max_inline_chars": {
          "type": "integer",
          "minimum": 512,
          "maximum": 20000,
          "default": 6000
        },
        "max_text_chars_per_match": {
          "type": "integer",
          "minimum": 50,
          "maximum": 2000,
          "default": 500
        },
        "cursor": {
          "type": "string",
          "maxLength": 32768
        },
        "diagnostics": {
          "type": "boolean",
          "default": false
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        }
      },
      "required": [
        "query"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_search",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_search result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_search_results_read",
    "canonical_name": "fs_search_results_read",
    "description": "Read a bounded page from an immutable filesystem search result reference.",
    "inputSchema": {
      "title": "fs_search_results_read arguments",
      "type": "object",
      "properties": {
        "ref": {
          "type": "string",
          "maxLength": 32768
        },
        "output_ref": {
          "type": "string",
          "maxLength": 32768
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 20000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 20000,
          "default": 4000
        }
      },
      "required": [
        "ref"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_search_results_read",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_search_results_read result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_grep_search",
    "canonical_name": "fs_grep_search",
    "description": "Search file contents under an allowed root using ripgrep with hard match and output-character budgets. Match-all patterns on a single file are refused unless allow_match_all is explicit.",
    "inputSchema": {
      "title": "fs_grep_search arguments",
      "type": "object",
      "properties": {
        "pattern": {
          "type": "string",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "default": ".",
          "maxLength": 32768
        },
        "path": {
          "type": "string",
          "description": "Compatibility alias for directory.",
          "maxLength": 32768
        },
        "glob": {
          "type": "string",
          "maxLength": 32768
        },
        "output_mode": {
          "type": "string",
          "enum": [
            "files_with_matches",
            "count_matches",
            "content"
          ],
          "default": "files_with_matches",
          "maxLength": 32768
        },
        "ignore": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "exclude": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 100,
          "default": 30
        },
        "max_matches": {
          "type": "integer",
          "minimum": 1,
          "maximum": 100,
          "default": 30
        },
        "max_output_chars": {
          "type": "integer",
          "minimum": 256,
          "maximum": 20000,
          "default": 4000
        },
        "allow_match_all": {
          "type": "boolean",
          "default": false
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        },
        "cache_policy": {
          "type": "string",
          "enum": [
            "auto",
            "snapshot",
            "refresh",
            "bypass"
          ],
          "default": "auto",
          "maxLength": 32768
        },
        "snapshot_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [
        "pattern"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_grep_search",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_grep_search result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_doctor",
    "canonical_name": "fs_doctor",
    "description": "Inspect local-filesystem MCP policy posture.",
    "inputSchema": {
      "title": "fs_doctor arguments",
      "type": "object",
      "properties": {},
      "required": [],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_doctor",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_doctor result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_patch_outcome_show",
    "canonical_name": "fs_patch_outcome_show",
    "description": "Read and durably reconcile the outcome for an fs_apply_patch operation_id.",
    "inputSchema": {
      "title": "fs_patch_outcome_show arguments",
      "type": "object",
      "properties": {
        "operation_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [
        "operation_id"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_patch_outcome_show",
      "readOnlyHint": false,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_patch_outcome_show result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  }
],
  "write": [
  {
    "name": "fs_guidance",
    "canonical_name": "fs_guidance",
    "description": "Show model-facing operating guidance for local-filesystem MCP workflows.",
    "inputSchema": {
      "title": "fs_guidance arguments",
      "type": "object",
      "properties": {
        "workflow": {
          "type": "string",
          "maxLength": 32768
        },
        "tool": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_guidance",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_guidance result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_read_file",
    "canonical_name": "fs_read_file",
    "description": "Read a text file under an allowed root with line offset and limit.",
    "inputSchema": {
      "title": "fs_read_file arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "offset": {
          "type": "integer",
          "minimum": 1,
          "maximum": 10000000,
          "default": 1,
          "description": "One-based first line to return."
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 1000,
          "default": 400,
          "description": "Maximum lines returned; paginate requests over 1,000 lines."
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 60000,
          "default": 5000
        }
      },
      "required": [
        "path"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_read_file",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_read_file result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_read_file_range",
    "canonical_name": "fs_read_file_range",
    "description": "Read a logical text-file line range under an allowed root. Lines are 1-based and inclusive; ranges over 1,000 lines return a bounded page with continuation.arguments for the same MCP tool.",
    "inputSchema": {
      "title": "fs_read_file_range arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "start_line": {
          "type": "integer",
          "minimum": 1,
          "maximum": 10000000,
          "description": "Inclusive logical start line."
        },
        "end_line": {
          "type": "integer",
          "minimum": 1,
          "maximum": 10000000,
          "description": "Inclusive logical end line. Requests spanning over 1,000 lines return a bounded page; follow continuation.arguments until has_more is false."
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 60000,
          "default": 5000
        }
      },
      "required": [
        "path",
        "start_line",
        "end_line"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_read_file_range",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_read_file_range result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_stat",
    "canonical_name": "fs_stat",
    "description": "Return file or directory metadata under an allowed root.",
    "inputSchema": {
      "title": "fs_stat arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        }
      },
      "required": [
        "path"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_stat",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_stat result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_glob_search",
    "canonical_name": "fs_glob_search",
    "description": "List files under an allowed root using ripgrep file globbing. Empty matches return ok with count 0.",
    "inputSchema": {
      "title": "fs_glob_search arguments",
      "type": "object",
      "properties": {
        "pattern": {
          "type": "string",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "default": ".",
          "maxLength": 32768
        },
        "ignore": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 500,
          "default": 100
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        },
        "cache_policy": {
          "type": "string",
          "enum": [
            "auto",
            "snapshot",
            "refresh",
            "bypass"
          ],
          "default": "auto",
          "maxLength": 32768
        },
        "snapshot_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [
        "pattern"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_glob_search",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_glob_search result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_repository_inventory",
    "canonical_name": "fs_repository_inventory",
    "description": "Return a bounded candidate-source inventory under an allowed root, excluding generated runtime artifacts by default. Use git-mcp for authoritative tracked and ignored state.",
    "inputSchema": {
      "title": "fs_repository_inventory arguments",
      "type": "object",
      "properties": {
        "pattern": {
          "type": "string",
          "default": "**/*",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "description": "Canonical inventory scope; mutually exclusive with root.",
          "maxLength": 32768
        },
        "root": {
          "type": "string",
          "description": "Compatibility alias for directory; mutually exclusive with directory.",
          "maxLength": 32768
        },
        "ignore": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "include_generated": {
          "type": "boolean",
          "default": false
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 500,
          "default": 100
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        },
        "cache_policy": {
          "type": "string",
          "enum": [
            "auto",
            "snapshot",
            "refresh",
            "bypass"
          ],
          "default": "auto",
          "maxLength": 32768
        },
        "snapshot_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_repository_inventory",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_repository_inventory result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_file_metrics",
    "canonical_name": "fs_file_metrics",
    "description": "Return bounded metadata-only file metrics under an allowed root.",
    "inputSchema": {
      "title": "fs_file_metrics arguments",
      "type": "object",
      "properties": {
        "pattern": {
          "type": "string",
          "default": "**/*",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "default": ".",
          "maxLength": 32768
        },
        "root": {
          "type": "string",
          "maxLength": 32768
        },
        "ignore": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "exclude": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 100,
          "default": 100
        },
        "max_bytes_per_file": {
          "type": "integer",
          "minimum": 1,
          "maximum": 1073741824,
          "default": 8388608
        },
        "max_total_scan_bytes": {
          "type": "integer",
          "minimum": 1,
          "maximum": 1073741824,
          "default": 268435456
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        },
        "cache_policy": {
          "type": "string",
          "enum": [
            "auto",
            "snapshot",
            "refresh",
            "bypass"
          ],
          "default": "auto",
          "maxLength": 32768
        },
        "snapshot_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_file_metrics",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_file_metrics result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_search",
    "canonical_name": "fs_search",
    "description": "Find matching lines, matching files, or per-file counts under one allowed directory with enforced item and response budgets.",
    "inputSchema": {
      "title": "fs_search arguments",
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "default": ".",
          "maxLength": 32768
        },
        "syntax": {
          "type": "string",
          "enum": [
            "literal",
            "regex"
          ],
          "default": "literal",
          "maxLength": 32768
        },
        "result_kind": {
          "type": "string",
          "enum": [
            "matches",
            "files",
            "counts"
          ],
          "default": "matches",
          "maxLength": 32768
        },
        "file_glob": {
          "type": "string",
          "maxLength": 32768
        },
        "exclude": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "case": {
          "type": "string",
          "enum": [
            "smart",
            "sensitive",
            "insensitive"
          ],
          "default": "smart",
          "maxLength": 32768
        },
        "max_results": {
          "type": "integer",
          "minimum": 1,
          "maximum": 100,
          "default": 20
        },
        "max_inline_chars": {
          "type": "integer",
          "minimum": 512,
          "maximum": 20000,
          "default": 6000
        },
        "max_text_chars_per_match": {
          "type": "integer",
          "minimum": 50,
          "maximum": 2000,
          "default": 500
        },
        "cursor": {
          "type": "string",
          "maxLength": 32768
        },
        "diagnostics": {
          "type": "boolean",
          "default": false
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        }
      },
      "required": [
        "query"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_search",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_search result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_search_results_read",
    "canonical_name": "fs_search_results_read",
    "description": "Read a bounded page from an immutable filesystem search result reference.",
    "inputSchema": {
      "title": "fs_search_results_read arguments",
      "type": "object",
      "properties": {
        "ref": {
          "type": "string",
          "maxLength": 32768
        },
        "output_ref": {
          "type": "string",
          "maxLength": 32768
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 20000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 20000,
          "default": 4000
        }
      },
      "required": [
        "ref"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_search_results_read",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_search_results_read result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_grep_search",
    "canonical_name": "fs_grep_search",
    "description": "Search file contents under an allowed root using ripgrep with hard match and output-character budgets. Match-all patterns on a single file are refused unless allow_match_all is explicit.",
    "inputSchema": {
      "title": "fs_grep_search arguments",
      "type": "object",
      "properties": {
        "pattern": {
          "type": "string",
          "maxLength": 32768
        },
        "directory": {
          "type": "string",
          "default": ".",
          "maxLength": 32768
        },
        "path": {
          "type": "string",
          "description": "Compatibility alias for directory.",
          "maxLength": 32768
        },
        "glob": {
          "type": "string",
          "maxLength": 32768
        },
        "output_mode": {
          "type": "string",
          "enum": [
            "files_with_matches",
            "count_matches",
            "content"
          ],
          "default": "files_with_matches",
          "maxLength": 32768
        },
        "ignore": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "exclude": {
          "type": "array",
          "items": {
            "type": "string",
            "maxLength": 32768
          },
          "maxItems": 256
        },
        "offset": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000,
          "default": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 100,
          "default": 30
        },
        "max_matches": {
          "type": "integer",
          "minimum": 1,
          "maximum": 100,
          "default": 30
        },
        "max_output_chars": {
          "type": "integer",
          "minimum": 256,
          "maximum": 20000,
          "default": 4000
        },
        "allow_match_all": {
          "type": "boolean",
          "default": false
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 60000
        },
        "cache_policy": {
          "type": "string",
          "enum": [
            "auto",
            "snapshot",
            "refresh",
            "bypass"
          ],
          "default": "auto",
          "maxLength": 32768
        },
        "snapshot_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [
        "pattern"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_grep_search",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_grep_search result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_doctor",
    "canonical_name": "fs_doctor",
    "description": "Inspect local-filesystem MCP policy posture.",
    "inputSchema": {
      "title": "fs_doctor arguments",
      "type": "object",
      "properties": {},
      "required": [],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_doctor",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_doctor result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_patch_outcome_show",
    "canonical_name": "fs_patch_outcome_show",
    "description": "Read and durably reconcile the outcome for an fs_apply_patch operation_id.",
    "inputSchema": {
      "title": "fs_patch_outcome_show arguments",
      "type": "object",
      "properties": {
        "operation_id": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [
        "operation_id"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_patch_outcome_show",
      "readOnlyHint": false,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_patch_outcome_show result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_write_file",
    "canonical_name": "fs_write_file",
    "description": "Write a text file under an allowed root and append an audit record. Refuses executable scripts under .ai/tmp or .ai/temp.",
    "inputSchema": {
      "title": "fs_write_file arguments",
      "type": "object",
      "properties": {
        "payload_ref": {
          "type": "string",
          "maxLength": 96
        },
        "payload_path": {
          "type": "string",
          "maxLength": 32768
        },
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "content": {
          "type": "string",
          "maxLength": 8388608
        },
        "overwrite": {
          "type": "boolean",
          "default": true
        },
        "create_only": {
          "type": "boolean",
          "default": false
        },
        "create_parent_directories": {
          "type": "boolean",
          "default": true
        },
        "timeout_ms": {
          "type": "integer",
          "default": 10000,
          "minimum": 0,
          "maximum": 300000
        },
        "expected_sha256": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_write_file",
      "readOnlyHint": false,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_write_file result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_str_replace_file",
    "canonical_name": "fs_str_replace_file",
    "description": "Replace exactly one string occurrence in a text file under an allowed root.",
    "inputSchema": {
      "title": "fs_str_replace_file arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "old": {
          "type": "string",
          "maxLength": 8388608
        },
        "new": {
          "type": "string",
          "maxLength": 8388608
        },
        "expected_sha256": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [
        "path",
        "old",
        "new"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_str_replace_file",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": false,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_str_replace_file result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_replace_range",
    "canonical_name": "fs_replace_range",
    "description": "Replace an inclusive line range in a text file under an allowed root.",
    "inputSchema": {
      "title": "fs_replace_range arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "start_line": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000
        },
        "end_line": {
          "type": "integer",
          "minimum": 0,
          "maximum": 10000000
        },
        "replacement": {
          "type": "string",
          "maxLength": 8388608
        },
        "expected_sha256": {
          "type": "string",
          "maxLength": 32768
        }
      },
      "required": [
        "path",
        "start_line",
        "end_line",
        "replacement"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_replace_range",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": false,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_replace_range result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_apply_patch",
    "canonical_name": "fs_apply_patch",
    "description": "Apply a unified diff or Codex-style patch atomically under allowed roots, with durable replay and recovery by operation_id.",
    "inputSchema": {
      "title": "fs_apply_patch arguments",
      "type": "object",
      "properties": {
        "patch": {
          "type": "string",
          "maxLength": 8388608
        },
        "operation_id": {
          "type": "string",
          "pattern": "^[A-Za-z0-9._-]{1,160}$",
          "maxLength": 160
        },
        "dry_run": {
          "type": "boolean",
          "default": false
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 300000,
          "default": 10000
        },
        "expected_sha256": {
          "type": "object",
          "maxProperties": 256,
          "additionalProperties": {
            "type": "string",
            "pattern": "^[0-9a-fA-F]{64}$",
            "maxLength": 64
          }
        }
      },
      "required": [
        "patch"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_apply_patch",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_apply_patch result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_move_path",
    "canonical_name": "fs_move_path",
    "description": "Move a file or directory under allowed roots.",
    "inputSchema": {
      "title": "fs_move_path arguments",
      "type": "object",
      "properties": {
        "from": {
          "type": "string",
          "maxLength": 32768
        },
        "to": {
          "type": "string",
          "maxLength": 32768
        },
        "overwrite": {
          "type": "boolean",
          "default": false
        },
        "expected_from_size": {
          "type": "integer",
          "minimum": 0,
          "maximum": 9007199254740991
        },
        "expected_from_mtime": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_from_sha256": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_from_tree_sha256": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_from_entry_count": {
          "type": "integer",
          "minimum": 0,
          "maximum": 5000000
        },
        "expected_to_size": {
          "type": "integer",
          "minimum": 0,
          "maximum": 9007199254740991
        },
        "expected_to_mtime": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_to_sha256": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_to_tree_sha256": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_to_entry_count": {
          "type": "integer",
          "minimum": 0,
          "maximum": 5000000
        },
        "expected_from": {
          "type": "object",
          "maxProperties": 5,
          "additionalProperties": false,
          "properties": {
            "mtime": {
              "type": "string",
              "maxLength": 128
            },
            "size": {
              "type": "integer",
              "minimum": 0,
              "maximum": 9007199254740991
            },
            "sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "tree_sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "entry_count": {
              "type": "integer",
              "minimum": 0,
              "maximum": 5000
            }
          }
        },
        "expected_to": {
          "type": "object",
          "maxProperties": 5,
          "additionalProperties": false,
          "properties": {
            "mtime": {
              "type": "string",
              "maxLength": 128
            },
            "size": {
              "type": "integer",
              "minimum": 0,
              "maximum": 9007199254740991
            },
            "sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "tree_sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "entry_count": {
              "type": "integer",
              "minimum": 0,
              "maximum": 5000
            }
          }
        }
      },
      "required": [
        "from",
        "to"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_move_path",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": false,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_move_path result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_create_directory",
    "canonical_name": "fs_create_directory",
    "description": "Create a directory under an allowed root.",
    "inputSchema": {
      "title": "fs_create_directory arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "recursive": {
          "type": "boolean",
          "default": false
        }
      },
      "required": [
        "path"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_create_directory",
      "readOnlyHint": false,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_create_directory result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_rename_directory",
    "canonical_name": "fs_rename_directory",
    "description": "Rename a directory under allowed roots.",
    "inputSchema": {
      "title": "fs_rename_directory arguments",
      "type": "object",
      "properties": {
        "from": {
          "type": "string",
          "maxLength": 32768
        },
        "to": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_from_mtime": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_from_size": {
          "type": "integer",
          "minimum": 0,
          "maximum": 9007199254740991
        },
        "expected_from_tree_sha256": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_from_entry_count": {
          "type": "integer",
          "minimum": 0,
          "maximum": 5000000
        },
        "expected_to_mtime": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_to_size": {
          "type": "integer",
          "minimum": 0,
          "maximum": 9007199254740991
        },
        "expected_to_tree_sha256": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_to_entry_count": {
          "type": "integer",
          "minimum": 0,
          "maximum": 5000000
        },
        "expected_from": {
          "type": "object",
          "maxProperties": 5,
          "additionalProperties": false,
          "properties": {
            "mtime": {
              "type": "string",
              "maxLength": 128
            },
            "size": {
              "type": "integer",
              "minimum": 0,
              "maximum": 9007199254740991
            },
            "sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "tree_sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "entry_count": {
              "type": "integer",
              "minimum": 0,
              "maximum": 5000
            }
          }
        },
        "expected_to": {
          "type": "object",
          "maxProperties": 5,
          "additionalProperties": false,
          "properties": {
            "mtime": {
              "type": "string",
              "maxLength": 128
            },
            "size": {
              "type": "integer",
              "minimum": 0,
              "maximum": 9007199254740991
            },
            "sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "tree_sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "entry_count": {
              "type": "integer",
              "minimum": 0,
              "maximum": 5000
            }
          }
        }
      },
      "required": [
        "from",
        "to"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_rename_directory",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": false,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_rename_directory result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  },
  {
    "name": "fs_delete_directory",
    "canonical_name": "fs_delete_directory",
    "description": "Delete a directory under an allowed root with explicit recursive consent.",
    "inputSchema": {
      "title": "fs_delete_directory arguments",
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "maxLength": 32768
        },
        "recursive": {
          "type": "boolean",
          "default": false
        },
        "expected_size": {
          "type": "integer",
          "minimum": 0,
          "maximum": 9007199254740991
        },
        "expected_mtime": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_tree_sha256": {
          "type": "string",
          "maxLength": 32768
        },
        "expected_entry_count": {
          "type": "integer",
          "minimum": 0,
          "maximum": 5000000
        },
        "expected": {
          "type": "object",
          "maxProperties": 5,
          "additionalProperties": false,
          "properties": {
            "mtime": {
              "type": "string",
              "maxLength": 128
            },
            "size": {
              "type": "integer",
              "minimum": 0,
              "maximum": 9007199254740991
            },
            "sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "tree_sha256": {
              "type": "string",
              "pattern": "^[0-9a-fA-F]{64}$",
              "maxLength": 64
            },
            "entry_count": {
              "type": "integer",
              "minimum": 0,
              "maximum": 5000
            }
          }
        }
      },
      "required": [
        "path"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "fs_delete_directory",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": false,
      "openWorldHint": false
    },
    "outputSchema": {
      "title": "fs_delete_directory result",
      "type": "object",
      "maxProperties": 256,
      "additionalProperties": true
    }
  }
],
} as unknown as Record<string, McpToolDefinition[]>;

export function nativeFilesystemTools(mode: string = 'write'): any[] {
  const selected = TOOLS[mode] ?? TOOLS["write"];
  return selected.map((tool) => ({ ...tool }));
}
