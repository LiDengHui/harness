export default {
  views: {
    agents: {
      title: 'Agents',
      intro:
        'One agent per {file} the server found. {tools} is the whitelist the agent may call — empty means every registered tool.',
      loading: 'loading…',
      empty: 'No agents were found in this workspace.',
      columns: {
        id: 'id',
        name: 'name',
        model: 'model',
        tools: 'tools',
        skills: 'skills',
        maxTokens: 'max tokens',
        source: 'source',
      },
      inherit: 'inherit',
      allTools: 'all',
      detail: {
        modelInherited: 'inherits the router default',
        unbounded: 'unbounded',
        everyTool: 'every registered tool',
        none: 'none',
      },
    },

    extensions: {
      title: 'Extensions',
      intro:
        'What the harness can reach for: skills it loads on demand, MCP servers it calls, and WASM plugins it runs in the sandbox.',
      loading: 'loading…',
      unavailable: {
        title: 'Nothing to show yet',
        explanation:
          'The extensions endpoint did not answer, so there is nothing to list here yet. The panel fills in as soon as the server serves it.',
      },
      empty: 'The endpoint answered with no skills, MCP servers or plugins registered.',

      skills: {
        heading: 'Skills',
        cost: 'metadata {metadata} tok · bodies {bodies} tok',
        disclosure: '({percent}% of the full price to know they exist)',
        empty: 'No skills registered.',
        inherit: 'inherit',
        columns: {
          name: 'name',
          description: 'description',
          model: 'model',
          allowedTools: 'allowed tools',
          metadataTokens: 'metadata tok',
          bodyTokens: 'body tok',
          source: 'source',
        },
      },

      mcp: {
        heading: 'MCP servers',
        configured: '{count} configured',
        empty: 'No MCP servers configured.',
        enabled: 'enabled',
        disabled: 'disabled',
        lazy: 'lazy',
        noTools: 'no tools advertised',
        kind: {
          stdio: 'A local process the server starts and talks to over stdin/stdout',
          http: 'An HTTP endpoint the server calls',
        },
      },

      plugins: {
        heading: 'WASM plugins',
        loaded: '{count} loaded',
        empty: 'No plugins loaded.',
        entry: 'entry {entry}',
        capabilities: 'capabilities',
        noCapabilities: 'none — the module imports nothing',
      },
    },

    entries: {
      handoff: 'handoff',
      subtask: 'subtask {status}',
      status: {
        pending: 'pending',
        running: 'running',
        succeeded: 'succeeded',
        failed: 'failed',
        skipped: 'skipped',
      },
    },
  },
};
