"""Personal Windows updates. Python standard library only; run inside Actions."""
from pathlib import Path
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch as mock_patch
import urllib.error
import urllib.parse
import urllib.request

OWNER = 'XuShall/cc-sessions-viewer'
UPSTREAM = 'jerrywu001/cc-sessions-viewer'
FEATURE_PATHS = ['src', 'src-tauri/src', 'test']
CONTROL_PATHS = ['.github/workflows', '.github/personal']
TAG_RE = re.compile(r'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z')
SHA_RE = re.compile(r'[0-9a-f]{40}\Z')
GUIDE = '''# Windows 更新操作

在仓库的 Actions 中选择「同步作者并发布 Windows」，点击 Run workflow。

- upstream_tag 留空：使用作者最新正式 Release；也可以填写 v0.6.3 这样的正式版本。
- rebuild 默认不勾选：没有新版本就跳过；勾选后为当前正式版生成新的 sortfix 修订。
- 成功后从自己的 Releases 下载 setup.exe 或 portable.zip。
- Windows 测试与编译均在 GitHub 云端进行，不需要本地安装 Node、Rust 或依赖。
- 回复优先、Codex 默认排序及恢复会话去重修复作为独立功能补丁保留。
- 每次更新后 main 直接接在作者的正式版提交之后，只保留一个定制提交。
- 代码冲突、测试失败或编译失败时不更新 main、不发布新版。查看失败步骤及日志；
  不要使用 Discard commits 来解决问题。
- 发布阶段因网络失败时，优先在该次运行选择 Re-run failed jobs，可继续完成上传。

## 可选的每日自动检查

默认关闭。需要时在仓库 Settings → Secrets and variables → Actions → Variables
新建仓库变量 AUTO_UPDATE，值为 true。每天北京时间 11:23 检查作者最新正式版；
没有新正式版时不编译、不发版。把变量改为 false 即可关闭。手动按钮始终可用。
GitHub 的定时任务可能延迟，公开仓库长期无活动时也可能被自动停用。

## 维护功能

同步脚本从当前 main 与作者基线之间提取 src、src-tauri/src 和 test 的差异，
使用 Git 三方合并迁移功能。版本号及 Windows 配置由脚本单独生成；Actions
配置完整保留自己的版本。功能变更仍应先验证，并保持 main 只有一个定制提交。
'''


def git(repo, *args, data=None, check=True):
    result = subprocess.run(['git', *args], cwd=repo, input=data, capture_output=True)
    if check and result.returncode:
        message = result.stderr.decode('utf-8', 'replace') + result.stdout.decode('utf-8', 'replace')
        raise RuntimeError('git ' + ' '.join(args[:3]) + ' failed:\n' + message[-10000:])
    return result.stdout if check else result


def rev(repo, value):
    return git(repo, 'rev-parse', value).decode().strip()


def summary(message):
    print(message, flush=True)
    if path := os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(path, 'a', encoding='utf-8') as output:
            output.write(message + '\n\n')


def outputs(**values):
    if path := os.environ.get('GITHUB_OUTPUT'):
        with open(path, 'a', encoding='utf-8') as output:
            for key, value in values.items():
                output.write(f'{key}={str(value).lower() if isinstance(value, bool) else value}\n')


def api(path, method='GET', body=None, binary=None):
    url = path if path.startswith('https://') else 'https://api.github.com/' + path
    if urllib.parse.urlparse(url).hostname not in ('api.github.com', 'uploads.github.com'):
        raise ValueError('Unexpected GitHub API host')
    headers = {'Accept': 'application/vnd.github+json', 'User-Agent': 'personal-windows-sync', 'X-GitHub-Api-Version': '2022-11-28'}
    if token := os.environ.get('GH_TOKEN'):
        headers['Authorization'] = 'Bearer ' + token
    data = binary if binary is not None else json.dumps(body).encode() if body is not None else None
    if data is not None:
        headers['Content-Type'] = 'application/octet-stream' if binary is not None else 'application/json'
    for attempt in range(3 if method == 'GET' else 1):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, data=data, headers=headers, method=method), timeout=60) as response:
                raw = response.read()
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            if error.code < 500 or attempt == 2 or method != 'GET':
                raise RuntimeError(f'GitHub API {method} returned HTTP {error.code}') from error
        except (urllib.error.URLError, TimeoutError, OSError):
            if attempt == 2 or method != 'GET':
                raise
        time.sleep(1)


def asset_names(version):
    return {f'Sessions.Viewer_{version}_x64-setup.exe', f'Sessions.Viewer_{version}_x64_portable.zip', 'SHA256SUMS.txt'}


def complete_release(release, version):
    return bool(release and not release['draft'] and asset_names(version) <= {a['name'] for a in release.get('assets', []) if a['size'] > 0})


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8', newline='\n')


def versions(repo, version):
    for name in ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json']:
        path = repo / name
        value = json.loads(path.read_text(encoding='utf-8'))
        value['version'] = version
        if name == 'package-lock.json':
            value['packages']['']['version'] = version
        if name.endswith('tauri.conf.json'):
            value['bundle']['targets'] = ['nsis']
            value['bundle']['createUpdaterArtifacts'] = False
            value['plugins']['updater']['endpoints'] = []
        write_json(path, value)
    for name, pattern in [
        ('src-tauri/Cargo.toml', r'(?ms)(^\[package\]\n.*?^version\s*=\s*)"[^"]+"'),
        ('src-tauri/Cargo.lock', r'(?m)(^name = "cc-sessions-viewer"\nversion = )"[^"]+"'),
    ]:
        path = repo / name
        text, count = re.subn(pattern, lambda m: m[1] + '"' + version + '"', path.read_text(encoding='utf-8'))
        if count != 1:
            raise RuntimeError('Unexpected version layout in ' + name)
        path.write_text(text, encoding='utf-8', newline='\n')


def controls_at(repo, old):
    names = git(repo, 'ls-tree', '-rz', '--name-only', old, '--', *CONTROL_PATHS).decode().split('\0')
    return {name: git(repo, 'show', old + ':' + name) for name in names if name}


def stage_candidate(repo, old, target, patch, controls, version):
    git(repo, 'checkout', '--detach', target)
    result = git(repo, 'apply', '--3way', '--index', '--binary', '-', data=patch, check=False)
    if result.returncode:
        conflicts = git(repo, 'diff', '--name-only', '--diff-filter=U').decode().strip()
        summary('功能补丁需要人工适配，main 和现有 Releases 未更改。\n\n' + (conflicts or result.stderr.decode('utf-8', 'replace')[-5000:]))
        raise RuntimeError('Feature patch did not merge cleanly')
    # Preserve the fork's workflow files byte-for-byte, including this updater.
    for name in git(repo, 'ls-tree', '-rz', '--name-only', target, '--', *CONTROL_PATHS).decode().split('\0'):
        if name:
            path = (repo / name).resolve()
            if not path.is_relative_to(repo.resolve()):
                raise RuntimeError('Control file escaped checkout')
            path.unlink(missing_ok=True)
    for name, data in controls.items():
        if name == '.github/personal/bootstrap.patch':
            continue
        path = repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    guide = repo / '.github/personal/README.md'
    guide.parent.mkdir(parents=True, exist_ok=True)
    guide.write_text(GUIDE, encoding='utf-8', newline='\n')
    versions(repo, version)
    git(repo, 'add', '--all')
    git(repo, 'diff', '--cached', '--check')
    # GITHUB_TOKEN may publish source refs; automation never rewrites workflows.
    git(repo, 'diff', '--cached', '--exit-code', old, '--', '.github/workflows')
    return git(repo, 'write-tree').decode().strip()


def prepare():
    repo = Path.cwd()
    if os.environ.get('GITHUB_REPOSITORY') != OWNER or os.environ.get('GITHUB_REF') != 'refs/heads/main':
        raise RuntimeError('Run this workflow on your own main branch')
    if git(repo, 'status', '--porcelain').strip():
        raise RuntimeError('Checkout is not clean')
    old = rev(repo, 'HEAD')
    tag = os.environ.get('UPSTREAM_TAG', '').strip()
    if not tag:
        release = api(f'repos/{UPSTREAM}/releases/latest')
        if not release or release['draft'] or release['prerelease']:
            raise RuntimeError('Upstream has no stable release')
        tag = release['tag_name']
    if not TAG_RE.fullmatch(tag):
        raise ValueError('upstream_tag must be a stable tag such as v0.6.3')
    git(repo, 'fetch', '--no-tags', f'https://github.com/{UPSTREAM}.git', 'refs/tags/' + tag)
    target = rev(repo, 'FETCH_HEAD^{commit}')
    base = git(repo, 'merge-base', old, target).decode().strip()
    upstream_version = json.loads(git(repo, 'show', target + ':package.json'))['version']
    if upstream_version != tag[1:]:
        raise RuntimeError('Upstream tag and package version differ')
    current_version = json.loads(git(repo, 'show', old + ':package.json'))['version']
    controls = controls_at(repo, old)
    seed = controls.get('.github/personal/bootstrap.patch')
    force = os.environ.get('REBUILD', 'false').lower() == 'true'
    same = base == target and not seed
    existing = api(f'repos/{OWNER}/releases/tags/v{current_version}') if same else None
    if same and not force and complete_release(existing, current_version):
        outputs(changed=False)
        summary(f'当前已包含作者 {tag}，安装包齐全，本次跳过编译和发布。\n\n[下载当前版本](https://github.com/{OWNER}/releases/latest)')
        return
    if git(repo, 'merge-base', '--is-ancestor', base, target, check=False).returncode:
        raise RuntimeError('Upstream history moved unexpectedly')
    if seed:
        header, patch = seed.split(b'\n', 1)
        seed_base = header.decode().removeprefix('# bootstrap-base: ')
        if not SHA_RE.fullmatch(seed_base):
            raise ValueError('Invalid bootstrap patch base')
        if git(repo, 'merge-base', '--is-ancestor', seed_base, target, check=False).returncode:
            raise RuntimeError('Bootstrap patch belongs to another upstream history')
    else:
        patch = git(repo, 'diff', '--binary', base, old, '--', *FEATURE_PATHS)
    prefix = upstream_version + '-sortfix.'
    revision = int(current_version[len(prefix):]) + 1 if current_version.startswith(prefix) and current_version[len(prefix):].isdigit() else 1
    version = current_version if same and not force else prefix + str(revision)
    release_tag = 'v' + version
    tree = stage_candidate(repo, old, target, patch, controls, version)
    result = Path(os.environ['PERSONAL_RESULT'])
    result.mkdir(parents=True, exist_ok=True)
    plan = {'old': old, 'target': target, 'upstream_tag': tag, 'tag': release_tag, 'version': version, 'tree': tree}
    write_json(result / 'plan.json', plan)
    (result / 'source.patch').write_bytes(git(repo, 'diff', '--cached', '--binary', target))
    outputs(changed=True, tag=release_tag, version=version)
    summary(f'候选版本：{release_tag}；作者基线：{tag}。\n\n完成测试和 Windows 编译后才会更新 main 并发布。')


def remote_ref(repo, ref):
    rows = git(repo, 'ls-remote', 'origin', ref).decode().splitlines()
    return rows[0].split()[0] if rows else ''


def verify_assets(result, version):
    names = asset_names(version)
    entries = {}
    for line in (result / 'SHA256SUMS.txt').read_text(encoding='utf-8-sig').splitlines():
        digest, name = line.split('  ', 1)
        if name not in names - {'SHA256SUMS.txt'} or not re.fullmatch('[0-9a-f]{64}', digest):
            raise RuntimeError('Unexpected checksum manifest')
        if hashlib.sha256((result / name).read_bytes()).hexdigest() != digest:
            raise RuntimeError('Asset checksum mismatch: ' + name)
        entries[name] = digest
    if set(entries) != names - {'SHA256SUMS.txt'}:
        raise RuntimeError('Windows assets are incomplete')
    return names


def publish():
    repo = Path.cwd()
    result = Path(os.environ['PERSONAL_RESULT'])
    plan = json.loads((result / 'plan.json').read_text(encoding='utf-8'))
    if os.environ.get('GITHUB_REPOSITORY') != OWNER or os.environ.get('GITHUB_REF') != 'refs/heads/main':
        raise RuntimeError('Publishing is restricted to your main branch')
    if rev(repo, 'HEAD') != plan['old'] or not all(SHA_RE.fullmatch(plan[key]) for key in ['old', 'target', 'tree']):
        raise RuntimeError('Invalid source plan')
    if not TAG_RE.fullmatch(plan['upstream_tag']) or not re.fullmatch(r'v\d+\.\d+\.\d+-sortfix\.[1-9]\d*', plan['tag']):
        raise RuntimeError('Invalid release tag')
    if plan['tag'] != 'v' + plan['version']:
        raise RuntimeError('Release version mismatch')
    names = verify_assets(result, plan['version'])
    git(repo, 'fetch', '--no-tags', f'https://github.com/{UPSTREAM}.git', 'refs/tags/' + plan['upstream_tag'])
    if rev(repo, 'FETCH_HEAD^{commit}') != plan['target']:
        raise RuntimeError('Upstream tag moved after verification')
    git(repo, 'checkout', '--detach', plan['target'])
    git(repo, 'apply', '--index', '--binary', '-', data=(result / 'source.patch').read_bytes())
    if git(repo, 'write-tree').decode().strip() != plan['tree']:
        raise RuntimeError('Tested and publishable source trees differ')
    git(repo, 'diff', '--cached', '--exit-code', plan['old'], '--', '.github/workflows')
    git(repo, 'config', 'user.name', 'Shiki')
    git(repo, 'config', 'user.email', '33885653+XuShall@users.noreply.github.com')
    git(repo, '-c', 'core.hooksPath=.git/no-hooks', 'commit', '--no-verify', '-m', 'feat(codex): keep reply-first sorting and one-click Windows updates')
    commit = rev(repo, 'HEAD')
    current = remote_ref(repo, 'refs/heads/main')
    tag_commit = remote_ref(repo, 'refs/tags/' + plan['tag'])
    if current == plan['old'] and not tag_commit:
        git(repo, 'push', '--atomic', '--force-with-lease=refs/heads/main:' + plan['old'], '--force-with-lease=refs/tags/' + plan['tag'] + ':', 'origin', 'HEAD:refs/heads/main', 'HEAD:refs/tags/' + plan['tag'])
    else:
        git(repo, 'fetch', '--no-tags', 'origin', 'refs/heads/main')
        if rev(repo, 'FETCH_HEAD^{tree}') != plan['tree'] or rev(repo, 'FETCH_HEAD^') != plan['target'] or tag_commit != current:
            raise RuntimeError('main or the release tag changed; no user work was overwritten')
        commit = current
    release = api(f'repos/{OWNER}/releases/tags/' + plan['tag'])
    if release and not release['draft']:
        if not complete_release(release, plan['version']):
            raise RuntimeError('Existing published release is incomplete; it was not overwritten')
        summary(f'发布已完成，保留现有下载文件：{release["html_url"]}')
        return
    body = f'''基于作者 [{plan['upstream_tag']}](https://github.com/{UPSTREAM}/releases/tag/{plan['upstream_tag']}) 的 Windows x64 定制版。

- 保留作者的新功能，以及“回复优先”选项和 Codex 默认回复排序。
- 保留恢复会话的当前文件选择修复，避免旧压缩副本遮住新回复。
- 在 GitHub Actions 中测试和打包；main 只保留一个自己的定制提交。
- 支持一键同步后续作者正式版，操作说明见 .github/personal/README.md。

下载 setup.exe 安装版或 portable.zip 便携版；SHA256SUMS.txt 包含校验值。
'''
    if not release:
        release = api(f'repos/{OWNER}/releases', method='POST', body={'tag_name': plan['tag'], 'target_commitish': commit, 'name': 'Sessions Viewer ' + plan['tag'] + ' (Windows)', 'body': body, 'draft': True, 'prerelease': False})
    for name in sorted(names):
        data = (result / name).read_bytes()
        digest = 'sha256:' + hashlib.sha256(data).hexdigest()
        previous = next((a for a in release.get('assets', []) if a['name'] == name), None)
        if previous and previous.get('digest') == digest and previous['size'] == len(data):
            continue
        if previous:
            api(f'repos/{OWNER}/releases/assets/{previous["id"]}', method='DELETE')
        uploaded = api(release['upload_url'].split('{')[0] + '?' + urllib.parse.urlencode({'name': name}), method='POST', binary=data)
        if not uploaded or uploaded['size'] != len(data) or uploaded.get('digest') not in (None, digest):
            raise RuntimeError('Uploaded asset verification failed')
    published = api(f'repos/{OWNER}/releases/{release["id"]}', method='PATCH', body={'draft': False, 'make_latest': 'true', 'body': body})
    if not complete_release(published, plan['version']):
        raise RuntimeError('GitHub release is incomplete')
    summary(f'更新成功：[{plan["tag"]}]({published["html_url"]})\n\n源码 `{commit}`；父提交 `{plan["target"]}`。main 仍只有一个自己的修改提交。\n\n下次更新：Actions → 同步作者并发布 Windows → Run workflow。')


class SyncTests(unittest.TestCase):
    def test_tag_validation(self):
        for tag in ['v0.6.3', 'v10.20.30']:
            self.assertIsNotNone(TAG_RE.fullmatch(tag))
        for tag in ['main', 'v1.2.3;echo bad', 'v1.2.3\n', 'v1.2.3-beta', '--all']:
            self.assertIsNone(TAG_RE.fullmatch(tag))

    def test_three_way_merge_preserves_both_features_and_one_parent(self):
        with tempfile.TemporaryDirectory(prefix='personal-sync-test-') as temporary:
            repo = Path(temporary)
            git(repo, 'init', '-b', 'main')
            git(repo, 'config', 'user.name', 'Synthetic Test')
            git(repo, 'config', 'user.email', 'test@example.invalid')
            (repo / 'src').mkdir()
            (repo / 'src/sort.txt').write_text('recent\n', encoding='utf-8')
            self.make_config(repo)
            git(repo, 'add', '.')
            git(repo, 'commit', '-m', 'upstream base')
            base = rev(repo, 'HEAD')
            (repo / 'src/sort.txt').write_text('recent\nreplyRecent\n', encoding='utf-8')
            control = repo / '.github/workflows/release.yml'
            control.parent.mkdir(parents=True)
            control.write_text('personal Windows workflow\n', encoding='utf-8')
            git(repo, 'add', '.')
            git(repo, 'commit', '-m', 'personal feature')
            old = rev(repo, 'HEAD')
            controls = controls_at(repo, old)
            patch = git(repo, 'diff', '--binary', base, old, '--', *FEATURE_PATHS)
            git(repo, 'checkout', '--detach', base)
            (repo / 'src/stars.txt').write_text('new upstream feature\n', encoding='utf-8')
            config = json.loads((repo / 'package.json').read_text())
            config['version'] = '1.2.4'
            config['dependencies']['upstream'] = '2.0'
            write_json(repo / 'package.json', config)
            git(repo, 'add', '.')
            git(repo, 'commit', '-m', 'new upstream')
            target = rev(repo, 'HEAD')
            stage_candidate(repo, old, target, patch, controls, '1.2.4-sortfix.1')
            self.assertIn('replyRecent', (repo / 'src/sort.txt').read_text())
            self.assertEqual((repo / 'src/stars.txt').read_text(), 'new upstream feature\n')
            self.assertEqual(json.loads((repo / 'package.json').read_text())['dependencies']['upstream'], '2.0')
            self.assertEqual(control.read_text(), 'personal Windows workflow\n')
            self.assertFalse(json.loads((repo / 'src-tauri/tauri.conf.json').read_text())['bundle']['createUpdaterArtifacts'])
            git(repo, 'commit', '-m', 'one customization')
            self.assertEqual(rev(repo, 'HEAD^'), target)
            self.assertEqual(git(repo, 'rev-list', '--count', target + '..HEAD').decode().strip(), '1')
            # Re-running on the latest completed personal release must not build,
            # create another commit, or write another release.
            completed = {'draft': False, 'assets': [{'name': name, 'size': 1} for name in asset_names('1.2.4-sortfix.1')]}
            current_head = rev(repo, 'HEAD')
            actual_git = git
            def offline_git(root, *args, **kwargs):
                if args == ('rev-parse', 'FETCH_HEAD^{commit}'):
                    return (target + '\n').encode()
                return b'' if args[0] == 'fetch' else actual_git(root, *args, **kwargs)
            original_directory = Path.cwd()
            try:
                os.chdir(repo)
                with mock_patch.dict(os.environ, {'GITHUB_REPOSITORY': OWNER, 'GITHUB_REF': 'refs/heads/main', 'UPSTREAM_TAG': 'v1.2.4', 'REBUILD': 'false'}), mock_patch(__name__ + '.api', return_value=completed), mock_patch(__name__ + '.git', side_effect=offline_git), mock_patch(__name__ + '.outputs') as captured, mock_patch(__name__ + '.summary'):
                    prepare()
                captured.assert_called_once_with(changed=False)
            finally:
                os.chdir(original_directory)
            self.assertEqual(rev(repo, 'HEAD'), current_head)
            self.assertFalse(git(repo, 'status', '--porcelain').strip())
            git(repo, 'checkout', '--detach', target)
            (repo / 'src/sort.txt').write_text('incompatible upstream rewrite\n', encoding='utf-8')
            git(repo, 'add', '.')
            git(repo, 'commit', '-m', 'conflicting upstream change')
            incompatible = rev(repo, 'HEAD')
            with mock_patch(__name__ + '.summary'), self.assertRaisesRegex(RuntimeError, 'Feature patch did not merge cleanly'):
                stage_candidate(repo, old, incompatible, patch, controls, '1.2.5-sortfix.1')
            self.assertEqual(rev(repo, 'HEAD'), incompatible)

    @staticmethod
    def make_config(repo):
        (repo / 'src-tauri').mkdir()
        write_json(repo / 'package.json', {'version': '1.2.3', 'dependencies': {'upstream': '1.0'}})
        write_json(repo / 'package-lock.json', {'version': '1.2.3', 'packages': {'': {'version': '1.2.3'}}})
        write_json(repo / 'src-tauri/tauri.conf.json', {'version': '1.2.3', 'bundle': {'targets': 'all', 'createUpdaterArtifacts': True}, 'plugins': {'updater': {'endpoints': ['https://example.invalid']}}})
        (repo / 'src-tauri/Cargo.toml').write_text('[package]\nname = "cc-sessions-viewer"\nversion = "1.2.3"\n', encoding='utf-8', newline='\n')
        (repo / 'src-tauri/Cargo.lock').write_text('[[package]]\nname = "cc-sessions-viewer"\nversion = "1.2.3"\n', encoding='utf-8', newline='\n')


if __name__ == '__main__':
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, 'reconfigure'):
            stream.reconfigure(encoding='utf-8')
    try:
        command = sys.argv[1]
        if command == 'self-test':
            unittest.main(argv=[sys.argv[0]], verbosity=2)
        elif command == 'prepare':
            prepare()
        elif command == 'publish':
            publish()
        else:
            raise ValueError('Expected prepare, publish or self-test')
    except Exception as error:
        summary('更新未完成：' + str(error))
        raise
