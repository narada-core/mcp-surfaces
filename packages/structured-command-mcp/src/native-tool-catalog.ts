import type { McpToolDefinition } from '@narada-core/mcp-fabric-contracts';

// Generated from the native tools/list registry. Do not hand-edit.
const TOOLS = {
  "all": [
  {
    "name": "structured_command_guidance",
    "description": "Guidance for argv-only structured command execution.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "workflow": {
          "type": "string"
        },
        "tool": {
          "type": "string"
        }
      },
      "additionalProperties": false
    },
    "annotations": {
      "title": "structured_command_guidance",
      "canonicalName": "structured_command_guidance",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "type": "object",
      "additionalProperties": true
    }
  },
  {
    "name": "structured_command_execution_policy_inspect",
    "description": "Inspect the policy governing structured command execution.",
    "inputSchema": {
      "type": "object",
      "additionalProperties": false
    },
    "annotations": {
      "title": "structured_command_execution_policy_inspect",
      "canonicalName": "structured_command_execution_policy_inspect",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "type": "object",
      "additionalProperties": true
    }
  },
  {
    "name": "structured_command_output_show",
    "description": "Read a materialized structured-command output ref with offset/limit paging.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "ref": {
          "type": "string"
        },
        "output_ref": {
          "type": "string"
        },
        "offset": {
          "type": "integer",
          "minimum": 0
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 20000
        }
      },
      "oneOf": [
        {
          "required": [
            "ref"
          ]
        },
        {
          "required": [
            "output_ref"
          ]
        }
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "structured_command_output_show",
      "canonicalName": "structured_command_output_show",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "type": "object",
      "additionalProperties": true
    }
  },
  {
    "name": "structured_command_execute",
    "description": "Execute a structured argv command under allowed-root and command policy, or read an existing execution_ref. Supply exactly one of command, input_ref, or execution_ref.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "input_ref": {
          "type": "string",
          "minLength": 1
        },
        "command": {
          "type": "string",
          "minLength": 1
        },
        "args": {
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "working_directory": {
          "type": "string"
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1
        },
        "wait_for_completion": {
          "type": "boolean"
        },
        "test_scope": {
          "type": "string"
        },
        "expected_cost": {
          "type": "string"
        },
        "stdout_offset": {
          "type": "integer",
          "minimum": 0
        },
        "stderr_offset": {
          "type": "integer",
          "minimum": 0
        },
        "stdout_limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 20000
        },
        "stderr_limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 20000
        },
        "observation_timeout_ms": {
          "type": "integer",
          "minimum": 1
        },
        "durable_process_lifetime_ms": {
          "type": "integer",
          "minimum": 1
        },
        "execution_ref": {
          "type": "string",
          "minLength": 1
        }
      },
      "oneOf": [
        {
          "required": [
            "command"
          ]
        },
        {
          "required": [
            "input_ref"
          ]
        },
        {
          "required": [
            "execution_ref"
          ]
        }
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "structured_command_execute",
      "canonicalName": "structured_command_execute",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": false,
      "openWorldHint": false
    },
    "outputSchema": {
      "type": "object",
      "additionalProperties": true
    }
  },
  {
    "name": "structured_command_start",
    "description": "Start a detached native command and return an execution_ref immediately. durable_process_lifetime_ms is the explicit kill deadline; observation_timeout_ms never terminates the process.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "input_ref": {
          "type": "string",
          "minLength": 1
        },
        "command": {
          "type": "string",
          "minLength": 1
        },
        "args": {
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "working_directory": {
          "type": "string"
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1
        },
        "wait_for_completion": {
          "type": "boolean"
        },
        "test_scope": {
          "type": "string"
        },
        "expected_cost": {
          "type": "string"
        },
        "stdout_offset": {
          "type": "integer",
          "minimum": 0
        },
        "stderr_offset": {
          "type": "integer",
          "minimum": 0
        },
        "stdout_limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 20000
        },
        "stderr_limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 20000
        },
        "observation_timeout_ms": {
          "type": "integer",
          "minimum": 1
        },
        "durable_process_lifetime_ms": {
          "type": "integer",
          "minimum": 1
        }
      },
      "oneOf": [
        {
          "required": [
            "command"
          ]
        },
        {
          "required": [
            "input_ref"
          ]
        }
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "structured_command_start",
      "canonicalName": "structured_command_start",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": false,
      "openWorldHint": false
    },
    "outputSchema": {
      "type": "object",
      "additionalProperties": true
    }
  },
  {
    "name": "structured_command_execution_show",
    "description": "Read one durable structured command execution by execution_ref.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "execution_ref": {
          "type": "string",
          "minLength": 1
        },
        "stdout_offset": {
          "type": "integer",
          "minimum": 0
        },
        "stderr_offset": {
          "type": "integer",
          "minimum": 0
        },
        "stdout_limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 20000
        },
        "stderr_limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 20000
        }
      },
      "required": [
        "execution_ref"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "structured_command_execution_show",
      "canonicalName": "structured_command_execution_show",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "type": "object",
      "additionalProperties": true
    }
  },
  {
    "name": "structured_command_powershell_parse_check",
    "description": "Parse-check an allowed-root PowerShell script without admitting arbitrary execution.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string"
        },
        "working_directory": {
          "type": "string"
        },
        "timeout_ms": {
          "type": "integer"
        }
      },
      "required": [
        "path"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "structured_command_powershell_parse_check",
      "canonicalName": "structured_command_powershell_parse_check",
      "readOnlyHint": true,
      "destructiveHint": false,
      "idempotentHint": true,
      "openWorldHint": false
    },
    "outputSchema": {
      "type": "object",
      "additionalProperties": true
    }
  },
  {
    "name": "structured_command_input_create",
    "description": "Create a scoped structured command input ref.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "input_id": {
          "type": "string"
        },
        "command": {
          "type": "string",
          "minLength": 1
        },
        "args": {
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "working_directory": {
          "type": "string"
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1
        },
        "wait_for_completion": {
          "type": "boolean"
        },
        "test_scope": {
          "type": "string"
        },
        "expected_cost": {
          "type": "string"
        }
      },
      "required": [
        "command"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "structured_command_input_create",
      "canonicalName": "structured_command_input_create",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": false,
      "openWorldHint": false
    },
    "outputSchema": {
      "type": "object",
      "additionalProperties": true
    }
  },
  {
    "name": "structured_command_elevated_window_execute",
    "description": "On Windows, launch a policy-approved command in a visible elevated UAC window. Execution requires confirm_elevation=true.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "command": {
          "type": "string",
          "minLength": 1
        },
        "args": {
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "working_directory": {
          "type": "string",
          "minLength": 1
        },
        "confirm_elevation": {
          "type": "boolean"
        },
        "wait": {
          "type": "boolean"
        },
        "dry_run": {
          "type": "boolean"
        },
        "timeout_ms": {
          "type": "integer",
          "minimum": 1
        }
      },
      "required": [
        "command",
        "working_directory"
      ],
      "additionalProperties": false
    },
    "annotations": {
      "title": "structured_command_elevated_window_execute",
      "canonicalName": "structured_command_elevated_window_execute",
      "readOnlyHint": false,
      "destructiveHint": true,
      "idempotentHint": false,
      "openWorldHint": false
    },
    "outputSchema": {
      "type": "object",
      "additionalProperties": true
    }
  }
],
} as unknown as Record<string, McpToolDefinition[]>;

export function nativeStructuredCommandTools(mode = 'write'): any[] {
  const selected = TOOLS[mode] ?? TOOLS["all"];
  return selected.map((tool) => ({ ...tool }));
}
