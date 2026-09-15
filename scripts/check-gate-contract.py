#!/usr/bin/env python3
"""Validate the v1 design schemas and synthetic fixtures; no runtime gate proof."""
import copy
from datetime import datetime
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource

ROOT = Path(__file__).resolve().parent.parent
SCHEMAS = ROOT / 'crates/engine/schemas'
FIXTURES = SCHEMAS / 'fixtures/gate-v1'


def strict_json(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate JSON object key: ' + key)
            result[key] = value
        return result

    def invalid_constant(value):
        raise ValueError('invalid JSON number: ' + value)

    return json.loads(data, object_pairs_hook=unique, parse_constant=invalid_constant)


def digest(data):
    return 'sha256:' + hashlib.sha256(data).hexdigest()


def text_line_count(data):
    text = data.decode('utf-8')
    return text.count('\n') + int(bool(text) and not text.endswith('\n'))


def no_remote_schema(uri):
    raise ValueError('unregistered schema: ' + uri)


registry = Registry(retrieve=no_remote_schema)
schemas = {}
for path in sorted(SCHEMAS.glob('gate-*.v1.schema.json')):
    value = strict_json(path.read_bytes())
    Draft202012Validator.check_schema(value)
    registry = registry.with_resource(value['$id'], Resource.from_contents(value))
    schemas[value['$id']] = value
formats = FormatChecker()


@formats.checks('date-time', raises=(ValueError, TypeError))
def utc_seconds(value):
    # V1 narrows deadlines to UTC seconds, not arbitrary RFC3339 variants.
    if not isinstance(value, str):
        return True
    datetime.strptime(value, '%Y-%m-%dT%H:%M:%SZ')
    return True


def validator(kind):
    return Draft202012Validator(schemas['urn:kranz:gate:' + kind + ':1'],
                               registry=registry, format_checker=formats)


REQUEST = validator('request')
RESPONSE = validator('response')
EVIDENCE = validator('evidence')
CASES = strict_json((FIXTURES / 'cases.json').read_bytes())['positive']


def fixture_pair(request, response, folder):
    """Check fixture relationships/bytes, not production authorization or safe I/O."""
    REQUEST.validate(request)
    RESPONSE.validate(response)
    params = request['params']
    if request['id'] != params['attemptId'] or response['id'] != request['id']:
        raise ValueError('attempt correlation mismatch')
    if 'error' in response:
        raise ValueError('error is not a judged result')
    result = response['result']
    for key in ['evaluationId', 'attemptId', 'binding']:
        if result[key] != params[key]:
            raise ValueError('response binding mismatch: ' + key)
    evidence_ref = params['evidence']
    content = (folder / evidence_ref['path']).read_bytes()
    if digest(content) != evidence_ref['digest'] or len(content) != evidence_ref['bytes']:
        raise ValueError('manifest byte identity mismatch')
    if result['evidenceDigest'] != evidence_ref['digest']:
        raise ValueError('response evidence mismatch')
    manifest = strict_json(content)
    EVIDENCE.validate(manifest)
    if manifest['binding'] != params['binding'] or manifest['missionId'] != params['missionId']:
        raise ValueError('manifest binding mismatch')
    ids, paths = set(), set()
    subject = None
    for entry in manifest['artifacts']:
        ref = entry['content']
        if entry['id'] in ids or ref['path'] in paths:
            raise ValueError('duplicate artifact identity')
        ids.add(entry['id'])
        paths.add(ref['path'])
        data = (folder / ref['path']).read_bytes()
        if digest(data) != ref['digest'] or len(data) != ref['bytes']:
            raise ValueError('artifact byte identity mismatch')
        if entry['role'] == 'subject':
            if subject is not None:
                raise ValueError('duplicate subject')
            subject = strict_json(data)
            if digest(data) != params['binding']['subjectDigest']:
                raise ValueError('subject byte identity mismatch')
    if subject != params['subject']:
        raise ValueError('inline subject differs from judged subject')
    if params['stage'] == 'plan-approval' and subject['planDigest'] != params['binding']['planDigest']:
        raise ValueError('proposed plan binding mismatch')
    contents = {entry['id']:entry['content'] for entry in manifest['artifacts']}
    for finding in result.get('findings',[]):
        for anchor in finding['evidence']:
            ref = contents.get(anchor['artifactId'])
            if ref is None or ref['digest'] != anchor['digest']:
                raise ValueError('finding has no matching input artifact')
            if 'lineStart' in anchor:
                count = text_line_count((folder / ref['path']).read_bytes())
                if not 1 <= anchor['lineStart'] <= anchor['lineEnd'] <= count:
                    raise ValueError('finding line range is outside the artifact')


class GateV1Contract(unittest.TestCase):
    def example(self, stage='milestone-validation', result='pass'):
        folder = FIXTURES / stage
        return (strict_json((folder / 'request.json').read_bytes()),
                strict_json((folder / (result + '.json')).read_bytes()), folder)

    def test_gate_contract_v1_all_five_subjects_round_trip(self):
        self.assertEqual(len(CASES), 5)
        self.assertEqual({case['name'] for case in CASES},
                         {'plan-approval','command-permission','milestone-validation','final-gate','merge'})
        count = 0
        for case in CASES:
            request = strict_json((FIXTURES / case['request']).read_bytes())
            self.assertEqual(strict_json(json.dumps(request)), request)
            for path in case['responses']:
                with self.subTest(path=path):
                    response = strict_json((FIXTURES / path).read_bytes())
                    self.assertEqual(strict_json(json.dumps(response)), response)
                    fixture_pair(request, response, FIXTURES / case['name'])
                    count += 1
        self.assertEqual(count, 15)

    def test_gate_contract_v1_stage_subject_mismatch_is_invalid(self):
        request, _, _ = self.example()
        for stage in ['plan-approval','command-permission','final-gate','merge']:
            with self.subTest(stage=stage):
                changed = copy.deepcopy(request)
                changed['params']['stage'] = stage
                self.assertFalse(REQUEST.is_valid(changed))

    def test_gate_contract_v1_missing_or_unknown_bindings_are_invalid(self):
        request, response, _ = self.example()
        for key in request['params']['binding']:
            with self.subTest(key=key):
                changed = copy.deepcopy(request)
                del changed['params']['binding'][key]
                self.assertFalse(REQUEST.is_valid(changed))
        for data, validate, content in [(request,REQUEST,'params'),(response,RESPONSE,'result')]:
            changed = copy.deepcopy(data)
            changed[content]['schemaVersion'] = 2
            self.assertFalse(validate.is_valid(changed))

    def test_gate_contract_v1_forged_authority_fields_are_invalid(self):
        _, response, _ = self.example()
        for key,value in [('actor','human:operator'),('approved',True),('blocking',False),
                          ('waiver','all'),('stage','merge'),('kind','deterministic')]:
            with self.subTest(key=key):
                changed = copy.deepcopy(response)
                changed['result'][key] = value
                self.assertFalse(RESPONSE.is_valid(changed))

    def test_gate_contract_v1_escalation_and_error_cannot_fabricate_verdict(self):
        request, response, folder = self.example(result='escalate')
        fixture_pair(request,response,folder)
        response['result']['verdict'] = 'pass'
        self.assertFalse(RESPONSE.is_valid(response))
        for code in [-32700,-32600,-32601,-32602,-32603,1001,1002,1003,1004]:
            with self.subTest(code=code):
                error = {'jsonrpc':'2.0','id':request['id'],
                         'error':{'code':code,'message':'Synthetic failure.'}}
                self.assertTrue(RESPONSE.is_valid(error))
                with self.assertRaisesRegex(ValueError,'not a judged result'):
                    fixture_pair(request,error,folder)
                error['result'] = response['result']
                self.assertFalse(RESPONSE.is_valid(error))

    def test_gate_contract_v1_invalid_response_envelopes_fail(self):
        _, response, _ = self.example()
        bad = [[], {}, {'jsonrpc':'2.0','id':None,'result':response['result']},
               {'jsonrpc':'2.0','id':None,'error':{'code':1001,'message':'Invalid null ID.'}}]
        for data in bad:
            with self.subTest(data=data):
                self.assertFalse(RESPONSE.is_valid(data))
        for code in [-32700,-32600]:
            self.assertTrue(RESPONSE.is_valid({'jsonrpc':'2.0','id':None,
                                              'error':{'code':code,'message':'Unidentifiable request.'}}))

    def test_gate_contract_v1_wrong_response_correlations_fail(self):
        request, response, folder = self.example()
        changed = copy.deepcopy(response)
        changed['id'] = 'different-attempt'
        with self.assertRaisesRegex(ValueError,'correlation mismatch'):
            fixture_pair(request,changed,folder)
        for key in ['evaluationId','attemptId','evidenceDigest']:
            with self.subTest(key=key):
                changed = copy.deepcopy(response)
                changed['result'][key] = digest(b'wrong') if key.endswith('Digest') else 'different-id'
                with self.assertRaises(ValueError):
                    fixture_pair(request,changed,folder)
        for key in request['params']['binding']:
            with self.subTest(binding=key):
                changed = copy.deepcopy(response)
                changed['result']['binding'][key] = digest(b'wrong') if key.endswith('Digest') else 'different-workspace'
                with self.assertRaisesRegex(ValueError,'binding mismatch'):
                    fixture_pair(request,changed,folder)

    def test_gate_contract_v1_inline_subject_cannot_replace_judged_bytes(self):
        request, response, folder = self.example()
        request['params']['subject']['snapshotDigest'] = digest(b'new candidate')
        self.assertTrue(REQUEST.is_valid(request))
        with self.assertRaisesRegex(ValueError,'inline subject'):
            fixture_pair(request,response,folder)

    def test_gate_contract_v1_artifact_paths_are_relative_portable_labels(self):
        _, response, _ = self.example()
        for path in ['../secret','a/../secret','./x','a//b','/etc/passwd',r'C:\secret',
                     'C:secret',r'\\server\share','file:secret','a/b:stream','a\x00b','a/']:
            with self.subTest(path=path):
                changed = copy.deepcopy(response)
                changed['result']['artifacts'] = [{'path':path,'digest':digest(b'x'),'bytes':1}]
                self.assertFalse(RESPONSE.is_valid(changed))
        request, _, folder = self.example()
        request['params']['evidence']['path'] = 'outputs/manifest.json'
        self.assertFalse(REQUEST.is_valid(request))
        manifest = strict_json((folder / 'inputs/manifest.json').read_bytes())
        manifest['artifacts'][0]['content']['path'] = 'build/subject.json'
        self.assertFalse(EVIDENCE.is_valid(manifest))

    def test_gate_contract_v1_deadlines_and_limits_are_bounded(self):
        request, _, _ = self.example()
        for value in ['2030-02-30T00:00:00Z','2030-01-01T25:00:00Z','2030-01-01T00:00:00+01:00']:
            changed = copy.deepcopy(request)
            changed['params']['deadline'] = value
            self.assertFalse(REQUEST.is_valid(changed))
        for value in [0,3600001]:
            changed = copy.deepcopy(request)
            changed['params']['limits']['wallTimeMs'] = value
            self.assertFalse(REQUEST.is_valid(changed))

    def test_gate_contract_v1_labels_hashes_and_locations_use_exact_bytes(self):
        request, _, _ = self.example(stage='plan-approval')
        for keys in [('id',), ('params','binding','subjectDigest'),
                     ('params','subject','baseCommit','value'), ('params','evidence','path')]:
            with self.subTest(keys=keys):
                changed = copy.deepcopy(request)
                target = changed
                for key in keys[:-1]:
                    target = target[key]
                target[keys[-1]] += '\n'
                self.assertFalse(REQUEST.is_valid(changed))
        for data, expected in [(b'',0), (b'a',1), (b'a\n',1), (b'a\r\nb',2),
                               (b'a\rb',1), ('a\u2028b'.encode(),1), (b'\n\n',2)]:
            with self.subTest(data=data):
                self.assertEqual(text_line_count(data),expected)
        with self.assertRaises(UnicodeDecodeError):
            text_line_count(b'\xff')

    def test_gate_contract_v1_duplicate_keys_and_non_json_numbers_are_invalid(self):
        for data in ['{"id":"first","id":"second"}', '{"score":NaN}', '{"score":Infinity}', '{']:
            with self.subTest(data=data), self.assertRaises(ValueError):
                strict_json(data)

    def test_gate_contract_v1_changed_manifest_or_artifact_bytes_fail(self):
        request, response, folder = self.example()
        for relative in ['inputs/manifest.json','inputs/subject.json']:
            with self.subTest(path=relative), tempfile.TemporaryDirectory(prefix='kranz-gate-schema-') as tmp:
                copy = Path(tmp) / 'fixture'
                shutil.copytree(folder,copy)
                changed = copy / relative
                changed.write_bytes(changed.read_bytes() + b'\n')
                with self.assertRaisesRegex(ValueError,'byte identity mismatch'):
                    fixture_pair(request,response,copy)

    def test_gate_contract_v1_finding_attribution_is_checked_against_bytes(self):
        request, response, folder = self.example()
        anchor = {'artifactId':'subject','digest':request['params']['binding']['subjectDigest'],
                  'lineStart':1,'lineEnd':1}
        response['result']['findings'] = [{'id':'finding-1','severity':'high','summary':'Synthetic finding.',
                                           'evidence':[anchor]}]
        fixture_pair(request,response,folder)
        for key,value in [('artifactId','missing'),('digest',digest(b'wrong')),('lineStart',2),('lineEnd',2)]:
            with self.subTest(key=key):
                changed = copy.deepcopy(response)
                changed['result']['findings'][0]['evidence'][0][key] = value
                self.assertTrue(RESPONSE.is_valid(changed))
                with self.assertRaisesRegex(ValueError,'finding'):
                    fixture_pair(request,changed,folder)

    def test_gate_contract_v1_finding_requires_specific_evidence(self):
        _, response, _ = self.example()
        response['result']['findings'] = [{'id':'finding-1','severity':'high','summary':'Synthetic finding.',
             'evidence':[{'artifactId':'subject','digest':response['result']['binding']['subjectDigest'],
                          'lineStart':1,'lineEnd':1}]}]
        self.assertTrue(RESPONSE.is_valid(response))
        response['result']['findings'][0]['evidence'] = []
        self.assertFalse(RESPONSE.is_valid(response))


if __name__ == '__main__':
    unittest.main(verbosity=2)
