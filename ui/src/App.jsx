import React, { useState, useEffect, useCallback, useRef } from 'react';
import './App.css';

const DEFAULT_STATUS = {
  connected: false,
  address: null,
  server_name: null,
  fingerprint: null,
  forwarding_active: false,
  last_error: null,
  latency_ms: null,
  version: '0.2.0',
};

function shortFingerprint(fp) {
  if (!fp) return '';
  return fp.length > 23 ? `${fp.slice(0, 23)}...` : fp;
}

function App() {
  const [status, setStatus] = useState(DEFAULT_STATUS);
  const [servers, setServers] = useState([]);
  const [knownServers, setKnownServers] = useState([]);
  const [isSearching, setIsSearching] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);
  const [address, setAddress] = useState('');
  const [pin, setPin] = useState('');
  const [trustNewCert, setTrustNewCert] = useState(false);
  const [transfer, setTransfer] = useState(null);
  const [dragActive, setDragActive] = useState(false);
  const tauriReady = typeof window !== 'undefined' && !!window.__TAURI__;
  const statusRef = useRef(status);
  statusRef.current = status;

  const invokeTauri = useCallback(async (cmd, args = {}) => {
    if (window.__TAURI__ && window.__TAURI__.invoke) {
      return await window.__TAURI__.invoke(cmd, args);
    }
    throw new Error('Tauri bridge unavailable (not running inside the desktop app)');
  }, []);

  const fetchStatus = useCallback(async () => {
    try {
      const res = await invokeTauri('get_status');
      setStatus(res);
    } catch (err) {
      // Not running inside Tauri (e.g. plain browser preview) -- leave defaults.
    }
  }, [invokeTauri]);

  const fetchKnownServers = useCallback(async () => {
    try {
      const res = await invokeTauri('list_known_servers');
      setKnownServers(res);
    } catch (err) {
      console.error('Failed to load known servers:', err);
    }
  }, [invokeTauri]);

  useEffect(() => {
    fetchStatus();
    fetchKnownServers();
    const interval = setInterval(fetchStatus, 3000);
    return () => clearInterval(interval);
  }, [fetchStatus, fetchKnownServers]);

  // Live updates pushed from the Rust side, so the UI reflects the actual
  // session (reconnects, pairing errors, forwarding toggled by the hotkey on
  // the server, latency) instead of only what this client asked for.
  useEffect(() => {
    if (!tauriReady) return undefined;
    const unlisten = [];

    window.__TAURI__.event
      .listen('kvm://status', (event) => {
        const e = event.payload;
        setStatus((prev) => {
          const next = { ...prev };
          switch (e.type) {
            case 'connected':
              next.connected = true;
              next.server_name = e.server_name;
              next.fingerprint = e.fingerprint;
              next.last_error = null;
              break;
            case 'disconnected':
              next.connected = false;
              next.forwarding_active = false;
              next.last_error = e.reason;
              break;
            case 'forwarding_changed':
              next.forwarding_active = e.active;
              break;
            case 'latency':
              next.latency_ms = e.ms;
              break;
            case 'error':
              next.last_error = e.message;
              break;
            default:
              break;
          }
          return next;
        });
        if (e.type === 'disconnected' || e.type === 'error') {
          fetchStatus();
          fetchKnownServers();
        }
      })
      .then((fn) => unlisten.push(fn));

    window.__TAURI__.event
      .listen('kvm://transfer', (event) => {
        setTransfer(event.payload);
      })
      .then((fn) => unlisten.push(fn));

    window.__TAURI__.event
      .listen('tauri://file-drop', (event) => {
        setDragActive(false);
        const paths = event.payload || [];
        if (paths.length > 0) {
          handleSendFile(paths[0]);
        }
      })
      .then((fn) => unlisten.push(fn));

    window.__TAURI__.event
      .listen('tauri://file-drop-hover', () => setDragActive(true))
      .then((fn) => unlisten.push(fn));

    window.__TAURI__.event
      .listen('tauri://file-drop-cancelled', () => setDragActive(false))
      .then((fn) => unlisten.push(fn));

    return () => {
      unlisten.forEach((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tauriReady]);

  const handleDiscover = async () => {
    setIsSearching(true);
    try {
      const res = await invokeTauri('discover_servers');
      setServers(res);
    } catch (err) {
      console.error('Discovery error:', err);
    } finally {
      setIsSearching(false);
    }
  };

  const handleToggleForwarding = async () => {
    try {
      const active = await invokeTauri('toggle_forwarding');
      setStatus((prev) => ({ ...prev, forwarding_active: active }));
    } catch (err) {
      setStatus((prev) => ({ ...prev, last_error: String(err) }));
    }
  };

  const handleConnect = async (targetAddress) => {
    const addr = (targetAddress || address).trim();
    if (!addr) return;
    setIsConnecting(true);
    setStatus((prev) => ({ ...prev, last_error: null }));
    try {
      await invokeTauri('connect_to_server', {
        address: addr,
        pin: pin.trim() ? pin.trim() : null,
        trustNewCert,
      });
      setAddress(addr);
      await fetchStatus();
      await fetchKnownServers();
    } catch (err) {
      setStatus((prev) => ({ ...prev, last_error: String(err) }));
    } finally {
      setIsConnecting(false);
    }
  };

  const handleDisconnect = async () => {
    try {
      await invokeTauri('disconnect_from_server');
    } catch (err) {
      console.error('Disconnect failed:', err);
    } finally {
      await fetchStatus();
    }
  };

  const handleForget = async (addr) => {
    try {
      await invokeTauri('forget_server', { address: addr });
      await fetchKnownServers();
    } catch (err) {
      console.error('Forget failed:', err);
    }
  };

  const handleSendFile = async (filePath) => {
    if (!statusRef.current.connected) {
      setTransfer({
        file_name: filePath.split(/[\\/]/).pop(),
        sent: 0,
        total: 0,
        done: true,
        error: 'Not connected to a server',
      });
      return;
    }
    try {
      await invokeTauri('send_file_to_server', { filePath });
    } catch (err) {
      setTransfer({
        file_name: filePath.split(/[\\/]/).pop(),
        sent: 0,
        total: 0,
        done: true,
        error: String(err),
      });
    }
  };

  const handleDrag = (e) => {
    e.preventDefault();
    e.stopPropagation();
  };

  const transferPercent =
    transfer && transfer.total > 0 ? Math.round((transfer.sent / transfer.total) * 100) : null;

  return (
    <div className="kvm-container">
      {/* Header */}
      <header className="kvm-header">
        <div className="logo-group">
          <div className="logo-icon">⚡</div>
          <div>
            <h1 className="title">KVM-RS</h1>
            <span className="subtitle">v{status.version}</span>
          </div>
        </div>

        <div className="status-badge-container">
          <span className={`status-dot ${status.connected ? 'online' : 'offline'}`} />
          <span className="status-text">
            {status.connected
              ? `Connected to ${status.server_name || status.address}`
              : 'Disconnected'}
          </span>
        </div>
      </header>

      {status.last_error && (
        <div className="error-banner">
          <span>{status.last_error}</span>
        </div>
      )}

      {/* Main Grid */}
      <main className="kvm-main-grid">
        {/* Connection Card */}
        <section className="card connect-card">
          <div className="card-header">
            <h2>Connection</h2>
            {status.connected && status.latency_ms != null && (
              <span className="cap-tag">{status.latency_ms} ms</span>
            )}
          </div>

          <div className="connect-form">
            <input
              className="text-input"
              type="text"
              placeholder="host:port (e.g. 192.168.1.10:4000)"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              disabled={status.connected}
            />
            <input
              className="text-input"
              type="password"
              placeholder="Pairing PIN (leave blank if already paired)"
              value={pin}
              onChange={(e) => setPin(e.target.value)}
              disabled={status.connected}
            />
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={trustNewCert}
                onChange={(e) => setTrustNewCert(e.target.checked)}
                disabled={status.connected}
              />
              Trust this server's certificate (re-pin on mismatch)
            </label>

            {status.connected ? (
              <button className="btn btn-disconnect connect-submit" onClick={handleDisconnect}>
                Disconnect
              </button>
            ) : (
              <button
                className="btn btn-connect connect-submit"
                onClick={() => handleConnect()}
                disabled={isConnecting || !address.trim()}
              >
                {isConnecting ? 'Connecting...' : 'Connect'}
              </button>
            )}
          </div>

          {status.connected && (
            <div className="info-box">
              <p>Pinned fingerprint: {shortFingerprint(status.fingerprint)}</p>
            </div>
          )}
        </section>

        {/* Input Control Card */}
        <section className="card control-card">
          <div className="card-header">
            <h2>Input Forwarding</h2>
          </div>

          <div className="toggle-container">
            <button
              className={`action-btn toggle-btn ${status.forwarding_active ? 'active' : ''}`}
              onClick={handleToggleForwarding}
              disabled={!status.connected}
            >
              {status.forwarding_active ? 'Stop Forwarding (Active)' : 'Start Forwarding'}
            </button>
          </div>

          <div className="info-box">
            <p>
              {!status.connected
                ? 'Connect to a server to enable input forwarding.'
                : status.forwarding_active
                  ? 'Keyboard & mouse are captured and routed to the target server.'
                  : 'Input capture is currently idle. Forwarding can also be toggled with the hotkey configured on the server.'}
            </p>
          </div>
        </section>

        {/* Server Discovery Card */}
        <section className="card discovery-card">
          <div className="card-header">
            <h2>mDNS Service Discovery</h2>
            <button className="scan-btn" onClick={handleDiscover} disabled={isSearching}>
              {isSearching ? 'Scanning LAN...' : 'Scan Network'}
            </button>
          </div>

          <div className="server-list">
            {servers.length === 0 ? (
              <div className="empty-state">
                <p>No KVM servers discovered yet.</p>
                <button className="text-btn" onClick={handleDiscover}>
                  Click here to scan local network
                </button>
              </div>
            ) : (
              servers.map((s) => (
                <div className="server-item" key={s.address}>
                  <div className="server-info">
                    <span className="server-name">{s.name}</span>
                    <span className="server-addr">{s.address}</span>
                  </div>
                  <button
                    className="btn btn-connect"
                    onClick={() => handleConnect(s.address)}
                    disabled={isConnecting || status.connected}
                  >
                    Connect
                  </button>
                </div>
              ))
            )}
          </div>
        </section>

        {/* File Transfer Card */}
        <section className="card transfer-card">
          <div className="card-header">
            <h2>File Transfer & Sync</h2>
            <span className="cap-tag">TLS file channel</span>
          </div>

          <div
            className={`dropzone ${dragActive ? 'drag-active' : ''} ${!status.connected ? 'dropzone-disabled' : ''}`}
            onDragEnter={handleDrag}
            onDragOver={handleDrag}
            onDragLeave={handleDrag}
          >
            <div className="dropzone-icon">📁</div>
            <p className="dropzone-text">
              {status.connected
                ? 'Drag & drop a file here to send it to the paired server'
                : 'Connect to a server to send files'}
            </p>
            <span className="dropzone-sub">SHA-256 integrity verification & resume</span>
          </div>

          {transfer && (
            <div className={`transfer-notification ${transfer.error ? 'transfer-error' : ''}`}>
              <span>
                {transfer.error
                  ? `${transfer.file_name}: ${transfer.error}`
                  : transfer.done
                    ? `${transfer.file_name}: transfer complete`
                    : `${transfer.file_name}: ${transferPercent != null ? `${transferPercent}%` : `${transfer.sent} bytes`}`}
              </span>
              {!transfer.done && transferPercent != null && (
                <div className="progress-track">
                  <div className="progress-fill" style={{ width: `${transferPercent}%` }} />
                </div>
              )}
            </div>
          )}
        </section>

        {/* Known Servers Card */}
        <section className="card paired-card">
          <div className="card-header">
            <h2>Known Servers</h2>
            <span className="cap-tag">TLS pinned</span>
          </div>

          <div className="server-list">
            {knownServers.length === 0 ? (
              <div className="empty-state">
                <p>No known servers yet. Connect to one to remember it here.</p>
              </div>
            ) : (
              knownServers.map((s) => (
                <div className="server-item" key={s.address}>
                  <div className="server-info">
                    <span className="server-name">{s.name || s.address}</span>
                    <span className="server-addr">{s.address}</span>
                    <span className="server-addr">{shortFingerprint(s.fingerprint)}</span>
                  </div>
                  <button className="btn btn-revoke" onClick={() => handleForget(s.address)}>
                    Forget
                  </button>
                </div>
              ))
            )}
          </div>
        </section>
      </main>

      {/* Footer */}
      <footer className="kvm-footer">
        <span>{status.connected ? 'TLS 1.3 Encryption Active' : 'Not Connected'}</span>
        <span>•</span>
        <span>{status.connected ? 'Clipboard Sync Enabled' : 'Clipboard Sync Idle'}</span>
        <span>•</span>
        <span>Protocol v{status.version}</span>
      </footer>
    </div>
  );
}

export default App;
