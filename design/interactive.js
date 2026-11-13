(() => {
  'use strict';
  const $ = selector => document.querySelector(selector);
  const icon = '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.3" aria-hidden="true"><path d="m3 5 5 5-5 5m8 0h6"/></svg>';
  const status = {
    ready: ['gray', '○', '就绪'], running: ['blue', '●', '工作中'],
    done: ['green', '✓', '完成'], waiting: ['amber', '!', '待确认'],
    stopped: ['gray', '■', '已中断']
  };
  let sequence = 4;
  let projectSequence = 3;
  let activeId = 1;
  let activeProjectId = 1;
  let visitSequence = 1;
  let menuProjectId = null;
  let pickedDirectory = null;
  const projects = [
    {id: 1, name: 'backend', path: '~/Projects/backend', expanded: true},
    {id: 2, name: 'website', path: '~/Projects/website', expanded: false},
    {id: 3, name: 'scripts', path: '~/Projects/scripts', expanded: true}
  ];
  let dialogAction = null;
  let toastTimer;
  const session = (id, name, state, entries = []) => ({
    id, name, state, entries, unread: false, draft: '', scroll: 0, timers: [], run: 0
  });
  const sessions = [
    session(1, '修改接口', 'ready', [
      {kind: 'prompt', text: '给用户列表接口增加分页，保持现有响应结构。'},
      {kind: 'answer', text: '我会先检查接口和调用方，再补充分页实现与边界测试。'},
      {kind: 'activity', text: '已检查接口与调用方', detail: 'Read src/api/users.ts\nRead src/services/user-service.ts'}
    ]),
    session(2, '检查代码', 'done', [
      {kind: 'prompt', text: '检查用户模块，重点看分页边界。'},
      {kind: 'activity', text: '已完成检查', detail: 'Reviewed src/api/users.ts\nReviewed tests/users.test.ts'},
      {kind: 'answer', text: '演示结果：发现一处需要补充的边界校验。\n当 page 小于 1 时，应返回明确的参数错误。\n\n可以继续输入要求，体验下一轮任务。'}
    ]),
    session(3, '编写测试', 'waiting', [
      {kind: 'prompt', text: '补充用户列表的分页测试，并运行验证。'},
      {kind: 'activity', text: '演示测试已准备', detail: '默认分页 · 空列表 · 超出末页 · 无效参数'},
      {kind: 'answer', text: '需要你确认后，才能继续演示测试执行。'}
    ]),
    session(4, '新会话', 'ready')
  ];
  sessions.forEach(s => { s.projectId = s.id === 4 ? 2 : 1; s.lastVisited = s.id === 1 ? 1 : 0; });
  sessions[1].unread = true;

  function node(tag, className, text) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  }
  const current = () => sessions.find(s => s.id === activeId);
  const currentProject = () => projects.find(p => p.id === (current()?.projectId ?? activeProjectId));
  const children = project => sessions.filter(s => s.projectId === project.id);
  const busy = project => children(project).some(s => ['running', 'waiting', 'starting', 'stopping'].includes(s.state));
  const stateNode = s => node('span', `state ${status[s.state][0]}`, `${status[s.state][1]} ${status[s.state][2]}`);
  function toast(message) {
    clearTimeout(toastTimer);
    $('.toast').textContent = message;
    $('.toast').hidden = false;
    toastTimer = setTimeout(() => { $('.toast').hidden = true; }, 2800);
  }
  function cancelTimers(s) {
    s.timers.forEach(clearTimeout);
    s.timers = [];
    s.run++;
  }
  function saveView() {
    const s = current();
    if (s) s.scroll = $('.terminal-scroll').scrollTop;
  }
  function selectSession(id) {
    saveView();
    activeId = id;
    activeProjectId = current().projectId;
    currentProject().expanded = true;
    current().lastVisited = ++visitSequence;
    current().unread = false;
    hideMenu();
    renderList();
    renderTerminal();
    $('#task-input').focus({preventScroll: true});
  }
  function renderList() {
    const focusedKey = document.activeElement?.dataset.focusKey;
    const list = $('.project-tree');
    const fragment = document.createDocumentFragment();
    for (const project of projects) {
      const group = node('section', 'project-group');
      group.dataset.projectId = project.id;
      const row = node('div', 'project-row');
      const toggle = node('button', 'project-toggle');
      toggle.dataset.focusKey = `project-${project.id}`;
      toggle.dataset.toggleProject = project.id;
      toggle.setAttribute('aria-expanded', String(project.expanded));
      toggle.setAttribute('aria-controls', `project-children-${project.id}`);
      toggle.title = project.path;
      toggle.innerHTML = `<span class="project-chevron" aria-hidden="true">${project.expanded ? '⌄' : '›'}</span><svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v10H3z"/></svg>`;
      toggle.append(node('span', 'project-label', project.name));
      toggle.onclick = () => { project.expanded = !project.expanded; hideMenu(); renderList(); };
      const more = node('button', 'project-more', '···');
      more.dataset.focusKey = `menu-${project.id}`;
      more.dataset.projectMenu = project.id;
      more.setAttribute('aria-label', `${project.name} 项目操作`);
      more.setAttribute('aria-expanded', String(menuProjectId === project.id));
      more.onclick = () => { const next = menuProjectId === project.id ? null : project.id; hideMenu(); menuProjectId = next; renderList(); };
      row.append(toggle, more);
      group.append(row);
      const projectSessions = children(project);
      const waiting = projectSessions.filter(s => s.state === 'waiting').length;
      const working = projectSessions.filter(s => s.state === 'running').length;
      const unread = projectSessions.filter(s => s.unread).length;
      const summary = node('div', 'project-summary');
      if (waiting) summary.append(node('span', 'amber', `${waiting} 个待确认`));
      if (working) summary.append(node('span', 'blue', `${working} 个工作中`));
      if (unread) summary.append(node('span', 'green', `${unread} 个完成未读`));
      if (summary.childNodes.length) group.append(summary);
      if (menuProjectId === project.id) {
        const panel = node('div', 'project-actions');
        const remove = node('button', 'delete-project', '删除项目');
        remove.dataset.deleteProject = project.id;
        remove.disabled = busy(project);
        panel.append(remove);
        if (remove.disabled) panel.append(node('p', '', '请先结束工作中或待确认的任务'));
        remove.onclick = () => requestDeleteProject(project);
        group.append(panel);
      }
      const content = node('div', 'project-children');
      content.id = `project-children-${project.id}`;
      content.hidden = !project.expanded;
      const path = node('div', 'project-path', project.path);
      path.title = project.path;
      content.append(path);
      for (const s of projectSessions) {
      const button = node('button', `session${s.id === activeId ? ' active' : ''}`);
      button.type = 'button';
      button.dataset.sessionId = String(s.id);
      button.dataset.focusKey = `session-${s.id}`;
      button.setAttribute('aria-pressed', String(s.id === activeId));
      button.setAttribute('aria-label', `${s.name}，${status[s.state][2]}${s.unread ? '，有未读结果' : ''}`);
      const top = node('div', 'session-top');
      top.innerHTML = icon;
      top.append(node('span', '', s.name));
      const bottom = node('div', 'session-bottom');
      bottom.append(node('span', '', 'Codex'), node('span', 'divider-dot', '/'), stateNode(s));
      button.append(top, bottom);
      if (s.unread) button.append(node('span', 'unread'));
      button.onclick = () => selectSession(s.id);
      button.ondblclick = () => openNameDialog(s);
      content.append(button);
      }
      if (!projectSessions.length) content.append(node('p', 'no-agents', '还没有 Agent'));
      const add = node('button', 'add-agent', '＋ 新建 Agent');
      add.dataset.newAgent = project.id;
      add.dataset.focusKey = `add-${project.id}`;
      add.onclick = () => openNameDialog(null, project);
      content.append(add);
      group.append(content);
      fragment.append(group);
    }
    if (!projects.length) fragment.append(node('p', 'tree-empty', '添加一个项目目录，\n再开始你的第一个 Agent。'));
    list.replaceChildren(fragment);
    $('#project-count').textContent = projects.length;
    if (focusedKey) list.querySelector(`[data-focus-key="${focusedKey}"]`)?.focus({preventScroll: true});
  }
  function renderHeader() {
    const s = current();
    const project = currentProject();
    $('.header-name').replaceChildren(node('span', 'header-project', project?.name || '工作台'), node('span', 'breadcrumb-slash', '/'), node('span', 'header-session', s?.name || '新建 Agent'), node('span', 'header-type', s ? 'Codex' : ''));
    $('.terminal-footer .left').replaceChildren(node('b', '', project?.name || '未选择项目'));
    $('.terminal-footer .left').title = project?.path || '';
    $('#current-status').replaceChildren(...(s ? [stateNode(s)] : []));
    $('#session-menu-button').disabled = !s;
  }
  function renderEntry(entry) {
    if (entry.kind === 'prompt') {
      const row = node('div', 'prompt');
      row.append(node('span', 'chevron', '›'), node('span', '', entry.text));
      return row;
    }
    if (entry.kind === 'activity') {
      const row = node('div', 'activity');
      const title = node('div', 'activity-title');
      title.append(node('span', 'done', '✓'), node('span', '', entry.text));
      row.append(title);
      if (entry.detail) row.append(node('div', 'activity-lines', entry.detail));
      return row;
    }
    return node('div', 'answer term-note', entry.text);
  }
  function renderTerminal() {
    renderHeader();
    const s = current();
    const scroll = $('.terminal-scroll');
    scroll.replaceChildren();
    if (!s || !s.entries.length) {
      const empty = node('div', 'empty');
      empty.append(node('h2', '', s ? '准备好了。' : '从一个项目开始。'), node('p', '', s ? '在下方输入任务，按 Enter 开始。\n切换到其他会话后，演示任务仍会继续。' : currentProject() ? '点击项目下的「新建 Agent」，在该目录开始工作。' : '点击左侧「项目」旁的 ＋，选择一个本地目录。'));
      scroll.append(empty);
    }
    if (s) {
      const intro = node('div', 'terminal-intro');
      intro.append(node('span', 'mark', '›_'), node('span', '', 'Codex'), node('span', 'version', `/ ${currentProject().name}`));
      scroll.prepend(intro);
      s.entries.forEach(entry => scroll.append(renderEntry(entry)));
      if (s.state === 'running') {
        const work = node('div', 'working');
        work.append(node('span', 'working-mark', '✳'), node('span', '', '正在处理任务…'), node('span', 'elapsed', '可切换会话'));
        scroll.append(work);
      }
      if (s.state === 'waiting') {
        const approval = node('div', 'approval');
        approval.append(node('p', '', '等待确认：模拟运行测试，不会执行本地命令。'));
        const actions = node('div', 'approval-actions');
        const approve = node('button', 'small-btn primary', '继续执行');
        approve.id = 'approve-task';
        approve.onclick = () => startRun(s, true);
        const deny = node('button', 'small-btn', '取消任务');
        deny.id = 'deny-task';
        deny.onclick = () => stopRun(s);
        actions.append(approve, deny);
        approval.append(actions);
        scroll.append(approval);
      }
      scroll.scrollTop = s.scroll;
    }
    const input = $('#task-input');
    input.value = s?.draft || '';
    input.disabled = !s || s.state === 'waiting';
    input.placeholder = !s ? '先新建一个会话' : s.state === 'waiting' ? '请先处理上方确认请求' : s.state === 'running' ? '任务执行中，可以先写下一轮要求…' : '输入任务，按 Enter 开始…';
    updateComposer();
  }
  function updateComposer() {
    const s = current();
    const send = $('#send-task');
    send.classList.toggle('stop', s?.state === 'running');
    send.textContent = s?.state === 'running' ? '■' : '↑';
    send.setAttribute('aria-label', s?.state === 'running' ? '中断任务' : '发送任务');
    send.title = s?.state === 'running' ? '中断任务（Esc）' : '发送任务（Enter）';
    send.disabled = !s || s.state === 'waiting' || (s.state !== 'running' && !s.draft.trim());
    $('#input-hint').textContent = s?.state === 'running' ? 'Esc 中断 · 草稿会保留' : 'Enter 发送 · Shift + Enter 换行';
  }
  function refresh(s) {
    renderList();
    if (s.id !== activeId) return;
    const scroll = $('.terminal-scroll');
    const atBottom = scroll.scrollHeight - scroll.clientHeight - scroll.scrollTop < 70;
    s.scroll = scroll.scrollTop;
    renderTerminal();
    if (atBottom) scroll.scrollTop = scroll.scrollHeight;
  }
  function stopRun(s) {
    cancelTimers(s);
    s.state = 'stopped';
    s.entries.push({kind: 'answer', text: '任务已中断。已有内容保留，可以重新输入要求。'});
    refresh(s);
    $('#task-input').focus();
  }
  function startRun(s, approved = false) {
    cancelTimers(s);
    const run = s.run;
    s.state = 'running';
    s.unread = false;
    if (approved) s.entries.push({kind: 'answer', text: '已确认，继续执行演示任务。'});
    refresh(s);
    const phases = [
      [1800, {kind: 'activity', text: '已梳理任务范围', detail: '读取相关上下文 · 确认处理步骤'}],
      [4200, {kind: 'activity', text: approved ? '正在模拟运行测试' : '正在模拟处理你的要求', detail: '演示输出：任务在独立会话中持续推进'}],
      [8000, {kind: 'answer', text: '本轮演示已完成。\n真实工具会在这里保留 Agent 的实际输出；本原型没有修改文件或执行命令。'}]
    ];
    for (const [delay, entry] of phases) {
      s.timers.push(setTimeout(() => {
        // A closed or interrupted session must never receive a late completion.
        if (!sessions.includes(s) || s.run !== run) return;
        s.entries.push(entry);
        if (delay === 8000) {
          s.state = 'done';
          s.unread = s.id !== activeId || document.hidden;
          s.timers = [];
          $('#live-status').textContent = `${s.name}已完成`;
        }
        refresh(s);
      }, delay));
    }
  }
  function submitTask() {
    const s = current();
    if (!s || s.state === 'waiting') return;
    if (s.state === 'running') { toast('当前任务仍在执行，草稿已保留。'); return; }
    const text = s.draft.trim();
    if (!text) return;
    s.entries.push({kind: 'prompt', text});
    s.draft = '';
    startRun(s);
    $('.terminal-scroll').scrollTop = $('.terminal-scroll').scrollHeight;
    $('#task-input').focus();
  }
  function hideMenu() {
    menuProjectId = null;
    document.querySelectorAll('.project-actions').forEach(el => el.remove());
    document.querySelectorAll('[data-project-menu]').forEach(el => el.setAttribute('aria-expanded', 'false'));
    $('#session-menu').hidden = true;
    $('#session-menu-button').setAttribute('aria-expanded', 'false');
  }
  function openDialog({title, description, value, label, action}) {
    hideMenu();
    $('#dialog-title').textContent = title;
    $('#dialog-description').textContent = description;
    const input = $('#session-name');
    input.hidden = value === undefined;
    $('#name-label').hidden = input.hidden;
    input.required = !input.hidden;
    input.value = value ?? '';
    $('#dialog-submit').textContent = label;
    dialogAction = action;
    $('#session-dialog').showModal();
    if (!input.hidden) { input.focus(); input.select(); }
    else $('#dialog-cancel').focus();
  }
  function openNameDialog(s, project = currentProject()) {
    if (!project) return;
    openDialog({title: s ? '重命名会话' : '新建 Agent', description: `所属项目：${project.name}\n工作目录：${project.path}`, value: s?.name || `新会话 ${sequence + 1}`, label: s ? '保存' : '创建会话', action: name => {
      if (s) { s.name = name; renderList(); renderHeader(); }
      else if (projects.includes(project)) { const next = session(++sequence, name, 'ready'); next.projectId = project.id; sessions.push(next); selectSession(next.id); }
    }});
  }
  function closeSession(s) {
    const index = sessions.indexOf(s);
    cancelTimers(s);
    sessions.splice(index, 1);
    if (activeId === s.id) activeId = sessions[Math.min(index, sessions.length - 1)]?.id ?? null;
    activeProjectId = current()?.projectId ?? s.projectId;
    if (current()) current().unread = false;
    renderList();
    renderTerminal();
  }

  function requestDeleteProject(project) {
    if (busy(project)) { toast('项目仍有未结束的任务，请先中断或处理确认。'); return; }
    openDialog({title: `删除「${project.name}」？`, description: `从工作台移除该项目及其 ${children(project).length} 个 Agent 会话。\n磁盘目录和文件不会被删除。`, label: '删除项目', action: () => {
      // Check again at confirmation time; a stale menu must not authorize removal.
      if (!projects.includes(project)) return;
      if (busy(project)) { toast('项目已有未结束的任务，无法删除。'); return; }
      const removedActive = current()?.projectId === project.id || activeProjectId === project.id;
      children(project).forEach(s => { cancelTimers(s); sessions.splice(sessions.indexOf(s), 1); });
      projects.splice(projects.indexOf(project), 1);
      if (removedActive) {
        const recent = sessions.reduce((best, s) => !best || (s.lastVisited || 0) > (best.lastVisited || 0) ? s : best, null);
        activeId = recent?.id ?? null;
        activeProjectId = recent?.projectId ?? projects[0]?.id ?? null;
        if (recent) { recent.unread = false; currentProject().expanded = true; }
        renderTerminal();
      }
      renderList();
      toast('项目已从工作台移除，磁盘文件保持不变。');
    }});
  }
  function openProjectDialog() {
    hideMenu();
    pickedDirectory = null;
    $('#project-name').value = '';
    $('#directory-selection').textContent = '尚未选择目录';
    $('#create-project').disabled = true;
    $('#project-picker-hint').textContent = typeof window.showDirectoryPicker === 'function'
      ? '浏览器仅提供目录名，不提供完整路径；不会读取或上传文件。'
      : '当前浏览器不支持目录选择，请用 Chrome 或 Edge 打开此原型。';
    $('#pick-directory').disabled = typeof window.showDirectoryPicker !== 'function';
    $('#project-dialog').showModal();
  }
  async function pickDirectory() {
    const button = $('#pick-directory');
    button.disabled = true;
    try {
      // Keep the handle only for identity checks. Never enumerate or write its files.
      const handle = await window.showDirectoryPicker({mode: 'read'});
      for (const project of projects) {
        if (project.handle && await project.handle.isSameEntry(handle)) {
          project.expanded = true;
          const last = children(project).sort((a, b) => (b.lastVisited || 0) - (a.lastVisited || 0))[0];
          $('#project-dialog').close();
          if (last) selectSession(last.id);
          else { saveView(); activeId = null; activeProjectId = project.id; renderList(); renderTerminal(); }
          toast('该目录已添加，已定位到已有项目。');
          return;
        }
      }
      pickedDirectory = handle;
      $('#project-name').value = handle.name;
      $('#directory-selection').textContent = `已选择：${handle.name}`;
      $('#create-project').disabled = false;
      $('#project-name').focus();
    } catch (error) {
      if (error.name !== 'AbortError') $('#project-picker-hint').textContent = '目录选择未成功，请在 Chrome 或 Edge 中重新选择。';
    } finally { button.disabled = false; }
  }

  // Convert the static design sheet into a local, dependency-free interaction demo.
  $('.eyebrow span:last-child').textContent = 'INTERACTIVE PROTOTYPE / 01';
  $('.window-controls').replaceWith(node('span', 'demo-label', '交互演示 · 未连接 Agent'));
  $('.project').remove();
  $('.sessions-head').innerHTML = '<span>项目 <span id="project-count" style="margin-left:5px;color:#777e89"></span></span><button class="add" id="new-project" aria-label="新建项目" title="选择目录，新建项目">+</button>';
  document.querySelectorAll('.sidebar>.session').forEach(el => el.remove());
  const list = node('div', 'project-tree');
  list.setAttribute('aria-label', '项目与 Agent 会话');
  $('.sidebar-bottom').before(list);
  $('.sidebar-bottom').innerHTML = '<span>切换会话</span><span>Alt + 1–9</span>';
  $('.header-right').innerHTML = '<div id="current-status"></div><button class="more" id="session-menu-button" aria-label="会话操作" aria-expanded="false" aria-controls="session-menu">···</button>';
  const menu = node('div', 'session-menu');
  menu.id = 'session-menu';
  menu.hidden = true;
  menu.innerHTML = '<button id="rename-session">重命名</button><button id="close-session">关闭会话</button>';
  $('.session-header').append(menu);
  $('.terminal').innerHTML = '<div class="terminal-scroll" aria-label="终端演示输出" tabindex="0"></div><div class="input-area"><form class="input composer" id="task-form"><span class="chevron" aria-hidden="true">›</span><textarea id="task-input" rows="1" maxlength="4000" aria-label="任务输入" spellcheck="false"></textarea><button class="send" id="send-task" type="button" aria-label="发送任务">↑</button></form><div class="terminal-footer"><div class="left"><b>my-project</b><span>·</span><span>main</span></div><span id="input-hint"></span></div></div>';
  $('.caption>span').textContent = '项目管理目录，Agent 独立工作。收起项目后，任务仍会继续。';
  $('.caption .swatches').innerHTML = '<span>本地演示 · 刷新后重置</span>';
  const dialog = node('dialog');
  dialog.id = 'session-dialog';
  dialog.setAttribute('aria-labelledby', 'dialog-title');
  dialog.innerHTML = '<form id="session-form"><h2 id="dialog-title"></h2><p id="dialog-description"></p><label id="name-label" for="session-name">会话名称</label><input id="session-name" maxlength="40" autocomplete="off"><div class="dialog-actions"><button id="dialog-cancel" type="button" class="small-btn">取消</button><button id="dialog-submit" class="small-btn primary" type="submit">保存</button></div></form>';
  document.body.append(dialog);
  const projectDialog = node('dialog', 'project-dialog');
  projectDialog.id = 'project-dialog';
  projectDialog.setAttribute('aria-labelledby', 'project-dialog-title');
  projectDialog.innerHTML = '<form id="project-form"><h2 id="project-dialog-title">新建项目</h2><p>选择一个本地目录，在其中管理 Agent。</p><label>项目目录</label><button type="button" id="pick-directory" class="directory-picker"><span aria-hidden="true">＋</span> 选择目录…</button><div id="directory-selection" class="directory-selection">尚未选择目录</div><p id="project-picker-hint" class="picker-hint"></p><label for="project-name">项目名称</label><input id="project-name" maxlength="60" required placeholder="选择目录后自动填入" autocomplete="off"><div class="dialog-actions"><button type="button" id="cancel-project" class="small-btn">取消</button><button type="submit" id="create-project" class="small-btn primary" disabled>创建项目</button></div></form>';
  document.body.append(projectDialog);
  const toastNode = node('div', 'toast');
  toastNode.hidden = true;
  toastNode.setAttribute('role', 'status');
  const live = node('span', 'sr-only');
  live.id = 'live-status';
  live.setAttribute('role', 'status');
  document.body.append(toastNode, live);

  $('#new-project').onclick = openProjectDialog;
  $('#pick-directory').onclick = pickDirectory;
  $('#cancel-project').onclick = () => projectDialog.close();
  $('#project-name').oninput = event => event.target.setCustomValidity('');
  $('#project-form').onsubmit = event => {
    event.preventDefault();
    const name = $('#project-name').value.trim();
    if (!pickedDirectory) return;
    if (!name) { $('#project-name').setCustomValidity('请输入项目名称'); $('#project-name').reportValidity(); return; }
    saveView();
    const project = {id: ++projectSequence, name, path: `已选目录：${pickedDirectory.name}`, handle: pickedDirectory, expanded: true};
    projects.push(project);
    activeId = null;
    activeProjectId = project.id;
    projectDialog.close();
    renderList(); renderTerminal();
    $(`[data-project-id="${project.id}"]`).scrollIntoView({block: 'nearest'});
  };
  projectDialog.addEventListener('close', () => { pickedDirectory = null; $('#project-name').setCustomValidity(''); });
  $('#task-input').oninput = event => { const s = current(); if (s) s.draft = event.target.value; updateComposer(); };
  $('#task-input').onkeydown = event => {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing && event.keyCode !== 229) {
      event.preventDefault(); submitTask();
    }
  };
  $('#task-form').onsubmit = event => { event.preventDefault(); submitTask(); };
  $('#send-task').onclick = () => current()?.state === 'running' ? stopRun(current()) : submitTask();
  $('#session-menu-button').onclick = () => {
    menu.hidden = !menu.hidden;
    $('#session-menu-button').setAttribute('aria-expanded', String(!menu.hidden));
    if (!menu.hidden) $('#rename-session').focus();
  };
  $('#rename-session').onclick = () => openNameDialog(current());
  $('#close-session').onclick = () => {
    const s = current();
    hideMenu();
    openDialog({title: '关闭这个会话？', description: s.state === 'running' ? `「${s.name}」正在工作。关闭会中断演示任务并移除本次记录。` : `「${s.name}」的本次演示记录将被移除。`, label: '关闭会话', action: () => closeSession(s)});
  };
  $('#dialog-cancel').onclick = () => dialog.close();
  $('#session-name').oninput = event => event.target.setCustomValidity('');
  $('#session-form').onsubmit = event => {
    event.preventDefault();
    const name = $('#session-name').value.trim();
    if (!$('#session-name').hidden && !name) {
      $('#session-name').setCustomValidity('请输入会话名称');
      $('#session-name').reportValidity(); return;
    }
    const action = dialogAction;
    dialog.close();
    action?.(name);
  };
  dialog.addEventListener('close', () => { dialogAction = null; $('#session-name').setCustomValidity(''); });
  document.addEventListener('click', event => { if (!event.target.closest('.header-right,.session-menu,.project-more,.project-actions')) hideMenu(); });
  document.addEventListener('keydown', event => {
    if (event.isComposing || dialog.open || projectDialog.open) return;
    if (event.key === 'Escape') {
      if (!menu.hidden || menuProjectId !== null) { hideMenu(); $('#session-menu-button').focus(); }
      else if (current()?.state === 'running') stopRun(current());
    }
    if (event.altKey && !event.ctrlKey && !event.metaKey && /^Digit[1-9]$/.test(event.code)) {
      const s = sessions[Number(event.code.slice(-1)) - 1];
      if (s) { event.preventDefault(); selectSession(s.id); }
    }
  });
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && current()?.unread) { current().unread = false; renderList(); }
  });
  window.addEventListener('pagehide', () => sessions.forEach(cancelTimers));
  renderList();
  renderTerminal();
  startRun(sessions[0]);
})();
