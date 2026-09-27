"""Claude Code fixtures: real record envelopes, synthetic content."""
import json, os, sys, itertools

OUT = sys.argv[1]
os.makedirs(OUT, exist_ok=True)
SID = '5f0c2a1e-7b3d-4c9a-9e21-3d4b5a6c7d8e'
REPO = '/Users/user/Workspaces/sample'
WT = '/Users/user/Worktrees/sample/feature'
VERSION = '2.1.259'
MODEL = 'claude-fable-5-1'
_uuid = itertools.count(1)
_clock = itertools.count(0)

def uid():
    return f'00000000-0000-4000-8000-{next(_uuid):012d}'

def ts():
    n = next(_clock)
    return f'2026-09-04T02:{25 + n // 60:02d}:{n % 60:02d}.{(n * 137) % 1000:03d}Z'

class T:
    def __init__(self, sid=SID, cwd=REPO):
        self.sid, self.cwd, self.parent, self.lines = sid, cwd, None, []

    def envelope(self, kind, message, **extra):
        u = uid()
        r = {'parentUuid': self.parent, 'isSidechain': extra.pop('isSidechain', False)}
        if kind == 'user':
            r['promptId'] = '00000000-0000-4000-9000-000000000001'
        r['type'] = kind
        r['message'] = message
        r.update({k: v for k, v in extra.items() if k == 'isMeta'})
        r.update({'uuid': u, 'timestamp': ts(), 'userType': 'external', 'entrypoint': 'cli',
                  'cwd': self.cwd, 'sessionId': self.sid, 'version': VERSION, 'gitBranch': 'main'})
        r.update({k: v for k, v in extra.items() if k != 'isMeta'})
        self.parent = u
        self.lines.append(json.dumps(r, ensure_ascii=False, separators=(',', ':')))
        return r

    def user(self, content, **extra):
        return self.envelope('user', {'role': 'user', 'content': content}, **extra)

    def assistant(self, *blocks, stop='end_turn', msg_id='msg_01Sample', model=MODEL):
        return self.envelope('assistant', {
            'model': model, 'id': msg_id, 'type': 'message', 'role': 'assistant',
            'content': list(blocks), 'stop_reason': stop, 'stop_sequence': None,
            'usage': {'input_tokens': 2, 'output_tokens': 40}})

    def raw(self, record):
        self.lines.append(record if isinstance(record, str) else json.dumps(record, ensure_ascii=False, separators=(',', ':')))

    def write(self, name):
        with open(os.path.join(OUT, name), 'w') as f:
            f.write('\n'.join(self.lines) + '\n')

def text(t): return {'type': 'text', 'text': t}
def thinking(t, sig='c2lnbmF0dXJl'): return {'type': 'thinking', 'thinking': t, 'signature': sig}
def tool_use(i, name, inp): return {'type': 'tool_use', 'id': i, 'name': name, 'input': inp, 'caller': {'type': 'direct'}}
def tool_result(i, content, err=False):
    b = {'type': 'tool_result', 'content': content, 'tool_use_id': i}
    if err: b['is_error'] = True
    return b

def bookkeeping(t, kind, **fields):
    t.raw({'type': kind, **fields, 'sessionId': t.sid})

# basic: bookkeeping first, reminders stripped, thinking, paired tools, is_error, cwd move.
t = T()
t.raw({'type': 'last-prompt', 'lastPrompt': 'Add a greeting to the sample CLI', 'sessionId': SID})
t.raw({'type': 'permission-mode', 'permissionMode': 'default', 'sessionId': SID})
t.raw({'type': 'file-history-snapshot', 'messageId': uid(), 'snapshot': {'messageId': uid(), 'trackedFileBackups': {}, 'timestamp': '2026-09-04T02:24:59.000Z'}, 'isSnapshotUpdate': False})
t.user('<system-reminder>\nThe date is 2026-09-04.\n</system-reminder>\nAdd a greeting to the sample CLI')
t.raw({'parentUuid': t.parent, 'isSidechain': False, 'type': 'attachment', 'attachment': {'type': 'todo_reminder', 'content': []}, 'uuid': uid(), 'timestamp': ts(), 'cwd': REPO, 'sessionId': SID, 'version': VERSION})
t.assistant(thinking('The user wants a greeting. Look at main.rs first.'), msg_id='msg_01Basic1', stop='tool_use')
t.assistant(thinking(''), msg_id='msg_01Basic1', stop='tool_use')
t.assistant(tool_use('toolu_01Read', 'Read', {'file_path': f'{REPO}/src/main.rs'}), msg_id='msg_01Basic1', stop='tool_use')
t.user([tool_result('toolu_01Read', '1\tfn main() {}\n')], toolUseResult={'type': 'text'}, sourceToolAssistantUUID=t.parent)
t.assistant(tool_use('toolu_01Bash', 'Bash', {'command': 'cargo test', 'description': 'Run tests'}), msg_id='msg_01Basic2', stop='tool_use')
t.user([tool_result('toolu_01Bash', 'error: could not compile `sample`', err=True)], toolUseResult='Error: could not compile', sourceToolAssistantUUID=t.parent)
t.raw({'parentUuid': t.parent, 'isSidechain': False, 'type': 'system', 'subtype': 'informational', 'content': 'Compiling', 'level': 'info', 'uuid': uid(), 'timestamp': ts(), 'cwd': REPO, 'sessionId': SID, 'version': VERSION})
t.cwd = WT
t.assistant(tool_use('toolu_01Edit', 'Edit', {'file_path': f'{WT}/src/main.rs', 'old_string': 'fn main() {}', 'new_string': 'fn main() {\n    println!("hello");\n}'}), msg_id='msg_01Basic3', stop='tool_use')
t.user([tool_result('toolu_01Edit', [text('The file has been updated.')])], toolUseResult={'type': 'update'}, sourceToolAssistantUUID=t.parent)
t.assistant(text('Added a `println!` greeting. Tests failed to compile before the edit.'), msg_id='msg_01Basic4')
t.raw({'type': 'ai-title', 'aiTitle': 'Add greeting to sample CLI', 'sessionId': SID})
t.write('basic.jsonl')

# tool_edges: pending tool_use, orphan tool_result, redacted thinking, rich result content.
t = T(sid='5f0c2a1e-0000-4000-8000-00000000edge')
t.user('Search the docs for the config format')
t.assistant(tool_use('toolu_01Search', 'Grep', {'pattern': 'config', 'path': 'docs'}), stop='tool_use', msg_id='msg_01Edge1')
t.user([tool_result('toolu_01Search', [text('docs/config.md'), {'type': 'tool_reference', 'tool_name': 'Read'}, {'type': 'image', 'source': {'type': 'base64', 'media_type': 'image/png', 'data': 'iVBORw0KGgo='}}])], sourceToolAssistantUUID=t.parent)
t.user([tool_result('toolu_01Orphan', 'result with no matching tool_use')])
t.assistant({'type': 'redacted_thinking', 'data': 'ZW5jcnlwdGVk'}, text('Reading the file now.'), msg_id='msg_01Edge2', stop='tool_use')
t.assistant(tool_use('toolu_01Pending', 'Read', {'file_path': f'{REPO}/docs/config.md'}), stop='tool_use', msg_id='msg_01Edge2')
t.write('tool_edges.jsonl')

# local_command: caveat (isMeta), slash command with args, stdout and stderr.
t = T(sid='5f0c2a1e-0000-4000-8000-0000000000cd')
t.user('<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>', isMeta=True)
t.user('<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args>fable</command-args>')
t.user('<local-command-stdout>Set model to Fable 5.1</local-command-stdout>')
t.user('<command-name>/doctor</command-name>\n            <command-message>doctor</command-message>\n            <command-args></command-args>')
t.user('<local-command-stderr>doctor: config file not found</local-command-stderr>')
t.user('Now explain the build error')
t.assistant(text('The build fails because `main` is missing a semicolon.'))
t.write('local_command.jsonl')

# command_title: first user turn is a slash command whose args give the title.
t = T(sid='5f0c2a1e-0000-4000-8000-00000000717e')
t.user('<command-message>brainstorm</command-message>\n<command-name>/brainstorm</command-name>\n<command-args>Design the export format</command-args>')
t.user([text('Base directory for this skill: /Users/user/.claude/skills/brainstorm\n\nFollow the brainstorming process.')], isMeta=True)
t.assistant(text('What should the export contain?'))
t.write('command_title.jsonl')

# agent_reports: task notification, teammate message with trailing text, sidechain.
t = T(sid='5f0c2a1e-0000-4000-8000-0000000a9e47')
t.user('Survey the parser module in the background')
t.assistant(tool_use('toolu_01Agent', 'Agent', {'description': 'Survey parser', 'prompt': 'List the public items in parser.rs', 'run_in_background': True}), stop='tool_use', msg_id='msg_01Agent1')
t.user([tool_result('toolu_01Agent', [text('Async agent launched successfully.\nagentId: a1b2c3d4e5f6a7b8c')])], sourceToolAssistantUUID=t.parent)
t.envelope('assistant', {'model': MODEL, 'id': 'msg_01Side', 'type': 'message', 'role': 'assistant', 'content': [text('Sidechain work that never reaches the note.')], 'stop_reason': 'end_turn', 'stop_sequence': None, 'usage': {'input_tokens': 1, 'output_tokens': 1}}, isSidechain=True, agentId='a1b2c3d4e5f6a7b8c')
t.user('<task-notification>\n<task-id>a1b2c3d4e5f6a7b8c</task-id>\n<tool-use-id>toolu_01Agent</tool-use-id>\n<output-file>/private/tmp/claude-501/sample/tasks/a1b2c3d4e5f6a7b8c.output</output-file>\n<status>completed</status>\n<summary>Agent "Survey parser" completed</summary>\n<note>A background agent completed a task.</note>\n<result>parser.rs exposes `parse` and `Record`.</result>\n</task-notification>')
t.user('<teammate-message teammate_id="researcher" summary="Found the issue">\nThe bug is in the tokenizer.\n</teammate-message>\nThanks, please fix it')
t.assistant(text('Fixing the tokenizer.'))
t.write('agent_reports.jsonl')

# attachments: image and documents with and without a title.
t = T(sid='5f0c2a1e-0000-4000-8000-0000000a77ac')
t.user([{'type': 'image', 'source': {'type': 'base64', 'media_type': 'image/png', 'data': 'iVBORw0KGgo='}}, text('What is wrong in this screenshot?')])
t.user([{'type': 'document', 'source': {'type': 'base64', 'media_type': 'application/pdf', 'data': 'JVBERi0='}}, {'type': 'document', 'title': 'spec.pdf', 'source': {'type': 'base64', 'media_type': 'application/pdf', 'data': 'JVBERi0='}}, text('Compare with these')])
t.assistant(text('The button overlaps the header.'))
t.write('attachments.jsonl')

# snapshot_only: bookkeeping without sessionId, an empty session.
t = T()
t.lines = []
t.raw({'type': 'file-history-snapshot', 'messageId': uid(), 'snapshot': {'messageId': uid(), 'trackedFileBackups': {}, 'timestamp': '2026-09-04T02:24:59.000Z'}, 'isSnapshotUpdate': False})
t.raw({'type': 'summary', 'summary': 'Earlier session', 'leafUuid': uid()})
t.write('snapshot_only.jsonl')

# malformed: a cut-off line mid-file and a partial last line.
t = T(sid='5f0c2a1e-0000-4000-8000-0000000bad01')
t.user('Summarise the log')
t.raw('{"parentUuid":"00000000-0000-4000-8000-000000000999","isSidechain":false,"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"cut off mid')
t.assistant(text('The log shows two warnings.'))
t.raw('{"parentUuid":"00000000-0000-4000-8000-000000001000","type":"us')
t.write('malformed.jsonl')

# unknown: an unlisted record type and an unlisted content block.
t = T(sid='5f0c2a1e-0000-4000-8000-00000000f00d')
t.user('Hello')
t.raw({'type': 'hologram', 'sessionId': t.sid, 'timestamp': ts()})
t.assistant({'type': 'server_tool_use', 'id': 'srvtoolu_01', 'name': 'web_search', 'input': {'query': 'rust'}}, text('Hi.'))
t.write('unknown.jsonl')

# modern: synthetic notice, shell mode, stray output, current tools, MCP,
# a repeated result, a result-less notification, a compaction summary.
t = T(sid='5f0c2a1e-0000-4000-8000-000000c0ffee')
t.user('Check the repo')
t.assistant(text("You've hit your monthly spend limit"), model='<synthetic>', msg_id='msg_01Synth')
t.user('<bash-input>ls</bash-input>')
t.user('<bash-stdout>Cargo.toml</bash-stdout><bash-stderr></bash-stderr>')
t.user('What does it contain?')
t.user('<local-command-stdout>stray output</local-command-stdout>')
t.assistant(tool_use('toolu_01Agent', 'Agent', {'description': 'Survey tests', 'prompt': 'List the tests', 'subagent_type': 'Explore'}),
            tool_use('toolu_01Skill', 'Skill', {'skill': 'review', 'args': 'src'}), stop='tool_use', msg_id='msg_01Modern1')
t.user([tool_result('toolu_01Agent', 'Three tests.'), tool_result('toolu_01Skill', 'Launching skill: review')], sourceToolAssistantUUID=t.parent)
t.assistant(tool_use('toolu_01Ask', 'AskUserQuestion', {'questions': [{'question': 'Which module first?', 'header': 'Module', 'options': [{'label': 'parser'}, {'label': 'cli'}], 'multiSelect': False}]}), stop='tool_use', msg_id='msg_01Modern2')
t.user([tool_result('toolu_01Ask', 'User answered: parser')], sourceToolAssistantUUID=t.parent)
t.user([tool_result('toolu_01Ask', 'second answer')])
t.assistant(tool_use('toolu_01Mcp', 'mcp__plugin_x__server__search', {'query': 'parser', 'limit': 5}), stop='tool_use', msg_id='msg_01Modern3')
t.user([tool_result('toolu_01Mcp', [text('parser.rs')])], sourceToolAssistantUUID=t.parent)
t.user('<task-notification>\n<task-id>b1c2d3e4f</task-id>\n<summary>Monitor event: "CI run"</summary>\n<event>CI run completed: success</event>\n</task-notification>')
t.user('This session is being continued from a previous conversation that ran out of context.\n\nSummary:\n1. The user asked to check the repo.', isCompactSummary=True, isVisibleInTranscriptOnly=True)
t.assistant(text('Continuing with the parser.'), msg_id='msg_01Modern4')
t.write('modern.jsonl')
