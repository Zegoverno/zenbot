#!/usr/bin/env python3
"""Exercise detached cutover decisions with fake service/network/tool commands, no live changes."""
import os
import shlex
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[1]


class CutoverTest(unittest.TestCase):
    def run_cutover(self, fail_import=False, fail_health=False, busy=False):
        temp=tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root=Path(temp.name)
        home=root/'home'
        live=home/'.zenbot'
        stage=live/'upgrades/100'
        stage.mkdir(parents=True)
        (live/'bin').mkdir()
        (live/'env').write_text('DATABASE_URL=postgres://zen:example@127.0.0.1:5432/zen\n')
        (live/'version').write_text('old')
        (root/'postgres-source').write_text('original PostgreSQL durable state')
        service=root/'service'
        service.write_text('old-service')
        for b in ['zend','zen','zen-engine']:
            (live/'bin'/b).write_text('old-'+b)
            (stage/b).write_text('new-'+b)
        (stage/'commit').write_text('new')
        (stage/'zenbot.service').write_text('new-service')
        (stage/'compose.yaml').write_text('retained PostgreSQL source')
        (stage/'storage-tool/scripts').mkdir(parents=True)
        (stage/'storage-tool/scripts/storage.py').write_text('mocked below')
        commands=root/'commands'
        commands.mkdir()
        wrappers={
            'sudo':'exec "$@"',
            'docker':'exit 0',
            'sleep':'exit 0',
            'journalctl':'exit 0',
            'systemctl':'''echo "$*" >> "$TEST_ROOT/service.log"
case "$1" in restart) touch "$TEST_ROOT/started";; esac
exit 0''',
            'python3':'''printf '%s\n' "$@" >> "$TEST_ROOT/storage-args"
case "$2" in
 export-postgres) echo private-archive > "$3";;
 restore) [ "$TEST_FAIL_IMPORT" = 0 ] || exit 1; echo verified-sqlite > "$4";;
esac''',
            'curl':'''if [ -e "$TEST_ROOT/started" ] && [ "$TEST_FAIL_HEALTH" = 1 ]; then exit 1; fi
if [ "$TEST_BUSY" = 1 ]; then echo '{"ok":true,"busy":1}'; else echo '{"ok":true,"busy":0}'; fi''',
        }
        for name,body in wrappers.items():
            f=commands/name
            f.write_text('#!/usr/bin/env bash\n'+body+'\n')
            f.chmod(0o755)
        env={**os.environ,'HOME':str(home),'PATH':str(commands)+':'+os.environ['PATH'],'ZEN_UPGRADE_SERVICE_FILE':str(service),'TEST_ROOT':str(root),'TEST_FAIL_IMPORT':str(int(fail_import)),'TEST_FAIL_HEALTH':str(int(fail_health)),'TEST_BUSY':str(int(busy))}
        # Busy kernels must not be forcibly stopped. Override SECONDS deadline via an immediate
        # mock date is unnecessary here: test health-unavailable failure separately below.
        result=subprocess.run(['bash',str(ROOT/'scripts/apply-upgrade.sh'),str(stage)],env=env,capture_output=True,text=True,timeout=15)
        return root,live,service,result

    def test_staged_export_keeps_the_retained_compose_project(self):
        root,live,service,result=self.run_cutover()
        self.assertEqual(result.returncode,0,result.stderr)
        args=(root/'storage-args').read_text().splitlines()
        command=shlex.split(args[args.index('--psql-command')+1])
        self.assertEqual(command[:4],['docker','compose','--project-directory',str(ROOT/'deploy')])
        self.assertEqual(command[4:6],['-f',str(live/'upgrades/100/compose.yaml')])
        self.assertEqual(command[6:],['exec','-T','postgres','psql','-U','zen','-d','zen'])

    def test_import_failure_restarts_unchanged_postgres_before_sqlite_start(self):
        root,live,service,result=self.run_cutover(fail_import=True)
        self.assertEqual(result.returncode,1,result.stderr)
        self.assertIn('start zenbot',(root/'service.log').read_text())
        self.assertNotIn('restart zenbot',(root/'service.log').read_text())
        self.assertIn('postgres://',(live/'env').read_text())
        self.assertEqual((live/'bin/zend').read_text(),'old-zend')

    def test_failed_health_after_sqlite_start_never_rolls_back_to_postgres(self):
        root,live,service,result=self.run_cutover(fail_health=True)
        self.assertEqual(result.returncode,1,result.stderr)
        self.assertIn('sqlite://',(live/'env').read_text())
        self.assertEqual((live/'bin/zend').read_text(),'new-zend')
        self.assertEqual((root/'service.log').read_text().count('restart zenbot'),1)
        self.assertTrue((live/'zen.sqlite3').exists())
        self.assertEqual((root/'postgres-source').read_text(),'original PostgreSQL durable state')
        self.assertIn('automatic rollback to PostgreSQL is forbidden',(live/'upgrade.log').read_text())

    def test_successful_cutover_retains_archive_and_marks_sqlite_capable_install(self):
        root,live,service,result=self.run_cutover()
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertTrue((live/'bin/.sqlite-storage').exists())
        self.assertEqual(len(list((live/'backups').glob('*postgres-cutover.jsonl'))),1)
        self.assertEqual(service.read_text(),'new-service')
        self.assertEqual((root/'postgres-source').read_text(),'original PostgreSQL durable state')
        self.assertIn('sqlite://',(live/'env').read_text())


if __name__=='__main__':
    unittest.main()
