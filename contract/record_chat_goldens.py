"""Record pure Slack behavior from Python before checking the Rust implementation.

No Slack SDK initialization, provider credentials or network calls are involved.
"""
import dataclasses
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / 'backend'))
from agentflow.services.chat import slack
from agentflow.services.chat.base import AnswerOffer, AnswerPrompt, Button, Reply, TaskForm, TextForm
from agentflow.workflow.notifications import Notice, notices
from agentflow.workflow.task_details import task_details, SECTIONS
from agentflow.services.chat import ask
from unittest.mock import patch
from datetime import datetime, timezone


def records():
    cases = []

    def record(function, args, expected):
        cases.append({'function': function, 'args': args, 'expected': expected})

    for text in ['', '<!channel>&<https://evil|click>', '😀é汉字', '&amp;lt;', '<@U123|Rob> <https://host/path> <!here>']:
        for name in ['escape', 'unslack']:
            record(name, [text], getattr(slack, name)(text))
    for text, limit in [('', 2900), ('a' * 2898 + '&amp;tail', 2900),
                        ('😀' * 3000, 2900), ('a' * 1600 + '\n' + 'b' * 2000, 2900),
                        ('a' * 100 + '\n' * 4 + 'b' * 100, 80)]:
        record('chunks', [text, limit], slack.chunks(text, limit))
    snapshot = {'jiraKey': 'AF-42', 'jiraTitle': '<!here> & title', 'status': 'ACTIVE',
                'phase': 'Requirements', 'attention': [
                    {'nodeKey': 'Architect', 'label': 'Architect', 'state': 'WAITING_FOR_USER',
                     'action': 'Answer', 'questions': 2}], 'questions': [
                    {'uid': 'question-guid', 'nodeKey': 'Architect', 'actionable': True,
                     'message': '<!channel>\nChoose?', 'choices': ['first', '`second`']},
                    {'uid': 'old-guid', 'nodeKey': 'Architect', 'actionable': False}]}
    for name in ['render_title', 'render_status']:
        record(name, [snapshot, 'http://127.0.0.1:8000/tasks/AF-42'],
               getattr(slack, name)(snapshot, 'http://127.0.0.1:8000/tasks/AF-42'))
    for tasks in [[], [snapshot], [snapshot] * 27,
                  [{'jiraKey': 'AF-1', 'status': 'MERGED'}, {'jiraKey': 'AF-2', 'status': 'CANCELLED'}]]:
        record('render_list', [tasks], slack.render_list(tasks))
    notify = ['WAITING_FOR_USER', 'WAITING_FOR_APPROVAL', 'FAILED', 'INTERRUPTED', 'READY',
              'NEEDS_HUMAN_REVIEW', 'READY_FOR_PR', 'CANCELLED']
    for include in [True, False]:
        expected = {key: dataclasses.asdict(value) for key, value in notices(snapshot, notify, include).items()}
        record('notices', [snapshot, notify, include], expected)
    for status in ['ACTIVE', 'NEEDS_HUMAN_REVIEW', 'READY_FOR_PR', 'FAILED', 'CANCELLED']:
        sample = {**snapshot, 'status': status, 'questions': [], 'attention': [
            {'nodeKey': 'Implementer', 'state': 'INTERRUPTED'},
            {'nodeKey': 'Reviewer', 'state': 'FAILED', 'label': 'Reviewer'}]}
        record('notices', [sample, notify, False],
               {key: dataclasses.asdict(value) for key, value in notices(sample, notify, False).items()})
    # A PR into another branch than the task started from: announced with the PR notices, until it matches.
    for target, enabled in [('main', notify), ('main', ['FAILED']), ('release/candidate', notify), (None, notify)]:
        sample = {**snapshot, 'status': 'PR_CREATED', 'baseBranch': 'release/candidate', 'prBaseBranch': target,
                  'questions': [], 'attention': [{'nodeKey': 'Reviewer', 'state': 'FAILED', 'label': 'Reviewer'}]}
        record('notices', [sample, enabled, False],
               {key: dataclasses.asdict(value) for key, value in notices(sample, enabled, False).items()})
    notice = Notice('question:guid', 'Architect asked a question', '<!here>\nWhich?', ('`one`', '&two'), 'guid')
    for answerable in [True, False]:
        record('render_notice', [dataclasses.asdict(notice), answerable], slack.render_notice(notice, answerable))
        boundary = dataclasses.replace(notice, question='a\r\nb\rc\v d\f e\x85f\u2028g\u2029h\n')
        record('render_notice', [dataclasses.asdict(boundary), answerable], slack.render_notice(boundary, answerable))
    prompt = AnswerPrompt('AF-42', 'guid', ('one', '😀' * 80, '<!channel>'))
    record('notice_blocks', ['Question', dataclasses.asdict(prompt)], slack.notice_blocks('Question', prompt))
    # Decision trees: the top level, each branch's follow-up, and a bare tree (no risk, no notes).
    decision = {'context': '4,108 accounts <!here> & co', 'risk': 'high', 'choices': [
        {'label': 'Soft-delete, keep invoices', 'recommended': True, 'followUp': {
            'message': 'When are they <purged>?', 'choices': [
                {'label': 'After their invoices close', 'recommended': True, 'detail': 'A nightly job & purge'},
                {'label': 'Never', 'warning': 'Table grows', 'note': 'Legal <!channel> asked.', 'source': 'docs/privacy.md'}]}},
        {'label': '😀' * 80, 'followUp': {'message': 'Tell them?', 'choices': [{'label': 'Email'}, {'label': 'No email'}]}},
        {'label': 'Hard-delete everything', 'detail': 'd' * 200, 'warning': '&' * 120, 'note': '<' * 300,
         'source': 'https://example.com/' + 'p' * 180}]}
    for branch in [None, 0, 1]:
        tree = AnswerPrompt('AF-42', 'guid', tuple(c['label'] for c in decision['choices']), decision, branch)
        record('notice_blocks', ['Question', dataclasses.asdict(tree)], slack.notice_blocks('Question', tree))
    bare = AnswerPrompt('AF-42', 'guid', ('A', 'B'), {'context': None, 'risk': None,
                                                      'choices': [{'label': 'A'}, {'label': 'B'}]})
    record('notice_blocks', ['Question', dataclasses.asdict(bare)], slack.notice_blocks('Question', bare))
    for value in ['0.1', '12.3', '1', '1.', '.1', 'back', '1.2.3', '1234.1', '١.٢']:
        record('parse_path', [value], slack.parse_path(value))
    for question in [None, {'answered': True, 'answer': '<!here>', 'message': 'Which?'},
                     {'answered': False, 'message': 'Pending'}]:
        for include in [True, False]:
            record('render_resolved', [question, include], slack.render_resolved(question, include))
    for answer in ['<!channel> | &text', '😀' * 1990]:
        offer = AnswerOffer('AF-42', 'guid', answer)
        record('offer_blocks', [dataclasses.asdict(offer)], slack.offer_blocks(offer))
    buttons = (Button('run', 'Run Architect', 'AF-42|Architect'),
               Button('cancel', 'Cancel', 'AF-42', 'Cancel this task?'),
               Button('run', '😀' * 80, 'v' * 2100), Button('new_task', 'New task', ''))
    record('buttons_block', [[dataclasses.asdict(b) for b in buttons]], slack.buttons_block(buttons))
    for reply in [Reply('plain'), Reply('😀' * 3000), Reply('Actions', buttons=buttons),
                  Reply('Reply', offer=AnswerOffer('AF-42', 'guid', 'yes'))]:
        record('reply_blocks', [dataclasses.asdict(reply)], slack.reply_blocks(reply))
    for value, label in [('', 'empty'), ('v' * 151, 'long'), ('value', '😀' * 100), ('ok', '<name>')]:
        record('option', [value, label], slack.option(value, label))
    for value in ['', 'AF-42|guid|yes|more', 'AF-42|guid|reply', 'AF-42||yes']:
        for name in ['parse_offer', 'parse_reference']:
            record(name, [value], getattr(slack, name)(value))
    for search in [False, True]:
        for mode in ['direct', 'parallel']:
            form = TaskForm('AF-42', 'D:/Dev/Example', search, mode, 'main')
            record('task_modal', [dataclasses.asdict(form)], slack.task_modal(form))
    record('answer_modal', [[], {'jira_key': 'AF-42', 'question_uid': 'guid-é'}],
           slack.answer_modal([], {'jira_key': 'AF-42', 'question_uid': 'guid-é'}))
    form = TextForm('request_changes', 'AF-42', 'Request changes', '<!here> Intro', 'Feedback', True, 3000)
    record('text_modal', [dataclasses.asdict(form)], slack.text_modal(form))
    for values in [{}, {'ticket': {'value': {'value': 'AF-42'}},
                       'repository': {'agentflow_task_repository': {'selected_option': {'value': 'D:/Dev/Example'}}},
                       'branch': {'value': {'value': 'main'}}, 'mode': {'value': {'selected_option': {'value': 'parallel'}}}}]:
        record('task_draft', ['U-operator', values], dataclasses.asdict(slack.task_draft('U-operator', values)))
    detailed = {**snapshot, 'requirements': {'requirements': '<!here>\n' + 'line\n' * 10,
                'acceptanceCriteria': 'é' * 700, 'unknown': 'private'},
                'requirementsDigest': None, 'requirementsDraftDigest': 'draft',
                'statusMessage': 'private <!channel>',
                'nodes': [{'key': 'Architect', 'label': '', 'error': 'failure\n' * 10,
                           'usage': {'runs': 1, 'billedCostUsd': None, 'estimatedCostUsd': 12.5,
                                     'totalTokens': 12000}}],
                'findings': [{'status': 'open', 'blocking': True, 'severity': 'high',
                              'title': '<!here> finding', 'file': 'a`b.rs', 'line': 5}],
                'events': [{'timestamp': '2026-10-04T12:30:00Z', 'level': 'warning', 'message': 'notice'}],
                'jiraIssue': {'syncedAt': '2026-10-04T12:00:00Z',
                              'markdown': '# AF-42\n\n## Description\nDesc <!channel>\n\n## Comments (1)\n\n### Comment 1 - Rob & team\n\nText'} }
    for snap in [{**snapshot}, detailed, {**detailed, 'jiraIssue': {}},
                 {**detailed, 'jiraIssue': {'markdown': '# AF-42\nOld description'}},
                 {**detailed, 'requirementsDigest': 'approved'}]:
        for allowed in [[], ['activity'], list(SECTIONS)]:
            result = task_details(snap, allowed)
            record('task_details', [snap, allowed, None, 10], result)
            record('render_details', [snap, result, 'http://127.0.0.1:8000/tasks/AF-42'],
                   slack.render_details(snap, result, 'http://127.0.0.1:8000/tasks/AF-42'))
    for command in [None, '/agentflow']:
        record('render_help', [command], slack.render_help(command))
    for text in ['**bold** <!channel>', '**bold\nline**', 'plain']:
        record('render_ai_answer', [text], slack.render_ai_answer(text))
    documents = [('Requirements', '<!channel>\n' + 'é' * 21000), ('Custom', 'end')]
    record('render_requirements', ['AF-42', documents], slack.render_requirements('AF-42', documents))
    now = datetime(2026, 10, 4, 12, tzinfo=timezone.utc)
    class FixedClock:
        fromisoformat = datetime.fromisoformat
        @staticmethod
        def now(tz):
            return now
    with patch.object(slack, 'datetime', FixedClock):
        for since in [None, 'bad', '2026-10-04T11:55:00Z', '2026-10-04T09:00:00Z', '2026-10-01T09:00:00Z']:
            tasks = [{**snapshot, 'attention': [{**snapshot['attention'][0], 'since': since}]}]
            record('render_digest', [tasks, None, now.isoformat()], slack.render_digest(tasks, None))
    for output in ['bad', '{}', '{"result":" Answer ","total_cost_usd":0.5}', '{"is_error":true,"result":"private"}']:
        record('claude_answer', [output], ask.claude_answer(output))
    for output in ['bad', '{"type":"result","exitCode":0}',
                   '{"type":"assistant.message","data":{"content":"yes"}}\n{"type":"result","exitCode":0}',
                   '{"type":"tool.start"}\n{"type":"assistant.message","data":{"content":"private"}}\n{"type":"result","exitCode":0}',
                   '{"type":"assistant.message","data":{"content":"private","toolRequests":[{}]}}\n{"type":"result","exitCode":0}']:
        record('copilot_answer', [output], ask.copilot_answer(output))
    record('build_prompt', [{'allowedDetails': ['pr'], 'value': 'é'}, 'Why?', [['earlier', 'answer']]],
           ask.build_prompt({'allowedDetails': ['pr'], 'value': 'é'}, 'Why?', [('earlier', 'answer')]))
    return cases


if __name__ == '__main__':
    target = ROOT / 'contract/golden/chat.json'
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(records(), ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(f'Recorded {len(records())} Python Slack cases')
