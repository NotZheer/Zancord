// Debug Console Interceptor Buffer
window.__ZANCORD_LOGS__ = window.__ZANCORD_LOGS__ || [];
(function() {
  const origLog = console.log;
  const origWarn = console.warn;
  const origError = console.error;

  function formatArgs(args) {
    return Array.from(args).map(a => {
      if (typeof a === 'object') {
        try { return JSON.stringify(a); } catch (e) { return String(a); }
      }
      return String(a);
    }).join(' ');
  }

  console.log = function() {
    const msg = `[LOG ${new Date().toTimeString().split(' ')[0]}] ${formatArgs(arguments)}`;
    window.__ZANCORD_LOGS__.push(msg);
    origLog.apply(console, arguments);
  };
  console.warn = function() {
    const msg = `[WARN ${new Date().toTimeString().split(' ')[0]}] ${formatArgs(arguments)}`;
    window.__ZANCORD_LOGS__.push(msg);
    origWarn.apply(console, arguments);
  };
  console.error = function() {
    const msg = `[ERROR ${new Date().toTimeString().split(' ')[0]}] ${formatArgs(arguments)}`;
    window.__ZANCORD_LOGS__.push(msg);
    origError.apply(console, arguments);
  };
})();

let activeTailscaleIp = 'Detecting...';

document.addEventListener('DOMContentLoaded', () => {
  let isCamOn = true;
  let isMicOn = true;
  let isScreenSharing = false;
  let havenRtc = null;

  const topNav = document.getElementById('top-nav');
  const dockContainer = document.getElementById('dock-container');
  const appLayout = document.getElementById('app-layout');
  const tailscaleIpDisplay = document.getElementById('tailscale-ip-display');

  // Auto-Detect Tailscale P2P IP from Tauri IPC if available
  if (window.__TAURI__ && window.__TAURI__.core) {
    window.__TAURI__.core.invoke('get_tailscale_ip')
      .then(res => {
        if (res && res.ip) {
          activeTailscaleIp = res.ip;
          if (tailscaleIpDisplay) tailscaleIpDisplay.textContent = `Tailscale: ${res.ip}`;
          console.log('[TAURI TAILSCALE DETECTED]', res);
        }
      })
      .catch(err => console.log('Tauri Tailscale IP IPC fallback:', err));
  } else {
    activeTailscaleIp = '100.111.151.89';
    if (tailscaleIpDisplay) tailscaleIpDisplay.textContent = `Tailscale: ${activeTailscaleIp}`;
  }

  const btnToggleMic = document.getElementById('btn-toggle-mic');
  const btnToggleCam = document.getElementById('btn-toggle-cam');
  const btnToggleScreen = document.getElementById('btn-toggle-screen');
  const btnToggleSettingsSidebar = document.getElementById('btn-toggle-settings-sidebar');
  const btnToggleChat = document.getElementById('btn-toggle-chat');
  const btnLeave = document.getElementById('btn-leave');
  const btnCopyInvite = document.getElementById('btn-copy-invite');
  const btnCopyDebugLogs = document.getElementById('btn-copy-debug-logs');
  const btnTopCopyLogs = document.getElementById('btn-top-copy-logs');

  const settingsSidebar = document.getElementById('settings-sidebar');
  const inputDisplayName = document.getElementById('input-display-name');
  const selectMicDevice = document.getElementById('select-mic-device');
  const selectCamDevice = document.getElementById('select-cam-device');
  const selectSpeakerDevice = document.getElementById('select-speaker-device');

  const chatDrawer = document.getElementById('chat-drawer');
  const btnCloseChat = document.getElementById('btn-close-chat');

  const user1Card = document.getElementById('user1-card');
  const user1Video = document.getElementById('user1-video');
  const user1NameTag = document.getElementById('user1-name-tag');
  const user1Avatar = document.getElementById('user1-avatar');
  const user1MicStatus = document.getElementById('user1-mic-status');
  const user1AudioBar = document.getElementById('user1-audio-bar');

  const user2Card = document.getElementById('user2-card');
  const user2Video = document.getElementById('user2-video');
  const user2NameTag = document.getElementById('user2-name-tag');
  const user2Avatar = document.getElementById('user2-avatar');

  const overlayUser1Video = document.getElementById('overlay-user1-video');

  const hashRoom = window.location.hash.replace('#room=', '').trim();
  const currentRoom = hashRoom || 'duo-cinema-room';
  if (!window.location.hash) {
    window.location.hash = `#room=${currentRoom}`;
  }

  const savedName = localStorage.getItem('haven_username') || 'Ana';
  let currentUser = { id: null, username: savedName };
  inputDisplayName.value = savedName;
  updateLocalUserUI(savedName);

  function updateLocalUserUI(name) {
    user1NameTag.innerHTML = `<i class="fa-solid fa-user"></i> ${escapeHtml(name)}`;
    const initials = name.split(' ').map(n => n[0]).join('').substring(0, 2).toUpperCase() || 'U1';
    user1Avatar.textContent = initials;
  }

  inputDisplayName.addEventListener('input', (e) => {
    const newName = e.target.value.trim() || 'User 1';
    currentUser.username = newName;
    localStorage.setItem('haven_username', newName);
    updateLocalUserUI(newName);
    if (socket && socket.connected) {
      socket.emit('user-state-change', { username: newName });
    }
  });

  let socket = null;

  // Auto Start ZanCord Engine & Local Media Immediately
  initHaven();

  async function initHaven() {
    console.log('[ZANCORD STARTUP] Initializing engine & local media immediately...');

    const dummySocket = {
      on: () => {},
      emit: () => {},
      id: 'local-init-id'
    };

    havenRtc = new HavenWebRTC(dummySocket, currentUser, {
      onRemoteStreamAdded: handleRemoteStreamAdded,
      onRemoteStreamRemoved: handleRemoteStreamRemoved,
      onPeerStateChanged: handlePeerStateChanged,
      onAudioLevelUpdate: handleAudioLevelUpdate
    });

    try {
      const localStream = await havenRtc.initLocalMedia(true, true);
      if (localStream && localStream.getVideoTracks().length > 0) {
        user1Video.srcObject = localStream;
        overlayUser1Video.srcObject = localStream;
        user1Card.classList.add('video-on');
        user1Video.play().catch(e => console.log('Local video play error:', e));
      }
    } catch (err) {
      console.error('[ZANCORD STARTUP MEDIA ERROR]', err);
    }

    initSocketConnection();
  }

  async function initSocketConnection() {
    const targetHost = (window.location.hostname && window.location.hostname !== 'tauri.localhost' && window.location.hostname !== 'localhost' && window.location.hostname !== '127.0.0.1' && window.location.hostname !== '')
      ? window.location.hostname
      : '100.111.151.89';

    const isWeb = (window.location.protocol === 'https:' || window.location.protocol === 'http:') && window.location.hostname !== 'tauri.localhost';
    const socketServerUrl = isWeb
      ? `${window.location.protocol}//${window.location.host}`
      : `http://localhost:3000`;

    console.log('[ZANCORD CONNECTING TO SIGNALING SERVER]:', socketServerUrl);

    socket = io(socketServerUrl, {
      transports: ['websocket', 'polling'],
      reconnection: true,
      reconnectionAttempts: Infinity,
      reconnectionDelay: 1000
    });

    socket.on('connect', async () => {
      console.log(`[SOCKET CONNECTED] Socket ID: ${socket.id}, Room: ${currentRoom}`);
      currentUser.id = socket.id;
      if (havenRtc) {
        havenRtc.socket = socket;
        havenRtc.currentUser.id = socket.id;
        havenRtc.initSocketListeners();
        havenRtc.joinRoom(currentRoom);
      }
      populateDeviceOptions();
    });
  }

  btnToggleSettingsSidebar.addEventListener('click', () => {
    settingsSidebar.classList.toggle('closed');
  });

  async function populateDeviceOptions() {
    if (!havenRtc) return;
    const { mics, cams, speakers } = await havenRtc.getDevices();

    selectMicDevice.innerHTML = mics.map((m, i) => 
      `<option value="${m.deviceId}">${m.label || 'Microphone ' + (i + 1)}</option>`
    ).join('');

    selectCamDevice.innerHTML = cams.map((c, i) => 
      `<option value="${c.deviceId}">${c.label || 'Camera ' + (i + 1)}</option>`
    ).join('');

    selectSpeakerDevice.innerHTML = speakers.map((s, i) => 
      `<option value="${s.deviceId}">${s.label || 'Speaker/Headphones ' + (i + 1)}</option>`
    ).join('');
  }

  function handleRemoteStreamAdded(peerId, stream, username) {
    const peerName = username || 'User 2';
    user2NameTag.innerHTML = `<i class="fa-solid fa-user"></i> ${escapeHtml(peerName)}`;
    user2Card.classList.add('video-on');
    user2Video.srcObject = stream;
    user2Video.play().catch(e => console.warn('Remote video play:', e));
  }

  function handleRemoteStreamRemoved(peerId) {
    user2Card.classList.remove('video-on');
    user2Video.srcObject = null;
    user2NameTag.innerHTML = '<i class="fa-solid fa-user-group"></i> Waiting for User 2...';
  }

  function handlePeerStateChanged(peerId, peerObj) {
    if (peerObj.username) {
      user2NameTag.innerHTML = `<i class="fa-solid fa-user"></i> ${escapeHtml(peerObj.username)}`;
    }
  }

  function handleAudioLevelUpdate(target, level) {
    if (target === 'local') {
      user1AudioBar.style.width = `${level}%`;
    }
  }

  btnToggleMic.addEventListener('click', () => {
    isMicOn = !isMicOn;
    havenRtc.toggleMicrophone(isMicOn);
    btnToggleMic.classList.toggle('off', !isMicOn);
    user1MicStatus.classList.toggle('mic-muted', !isMicOn);
  });

  btnToggleCam.addEventListener('click', () => {
    isCamOn = !isCamOn;
    havenRtc.toggleCamera(isCamOn);
    btnToggleCam.classList.toggle('off', !isCamOn);
    user1Card.classList.toggle('video-on', isCamOn);
  });

  btnCopyInvite.addEventListener('click', () => {
    const inviteHost = (activeTailscaleIp && activeTailscaleIp !== 'Detecting...') ? activeTailscaleIp : '100.111.151.89';
    const inviteUrl = `https://${inviteHost}:3443/#room=${currentRoom}`;
    navigator.clipboard.writeText(inviteUrl);
    btnCopyInvite.innerHTML = '<i class="fa-solid fa-check"></i> <span>Copied!</span>';
    setTimeout(() => {
      btnCopyInvite.innerHTML = '<i class="fa-solid fa-link"></i> <span>Copy Link</span>';
    }, 2000);
  });

  const handleCopyLogs = () => {
    const logsText = window.__ZANCORD_LOGS__.join('\n');
    navigator.clipboard.writeText(logsText);
    alert('ZanCord Debug Logs copied to clipboard!');
  };

  btnCopyDebugLogs.addEventListener('click', handleCopyLogs);
  btnTopCopyLogs.addEventListener('click', handleCopyLogs);

  function escapeHtml(str) {
    return str.replace(/[&<>"']/g, (m) => ({
      '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#039;'
    })[m]);
  }
});
