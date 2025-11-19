import React, { useState } from 'react';
import './App.css';

function App() {
  const [isPanelOpen, setPanelOpen] = useState(true);

  return (
    <div className={`App ${isPanelOpen ? 'panel-open' : 'panel-closed'}`}>
      <div className="panel">
        <div className="panel-content">
          <h2>KVM-RS</h2>
          <div className="drop-area">
            <p>Drag & drop files here to transfer</p>
          </div>
        </div>
        <button className="toggle-button" onClick={() => setPanelOpen(!isPanelOpen)}>
          {isPanelOpen ? '<' : '>'}
        </button>
      </div>
    </div>
  );
}

export default App;
