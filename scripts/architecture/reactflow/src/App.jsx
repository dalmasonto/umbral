import React, { useCallback } from 'react';
import {
  ReactFlow, ReactFlowProvider, Background, BackgroundVariant, Controls, MiniMap,
  Panel, Handle, Position, useNodesState, useEdgesState,
  getNodesBounds, getViewportForBounds,
} from '@xyflow/react';
import { toPng } from 'html-to-image';
import { initialNodes, initialEdges, legendItems } from './graph.js';
import { blueprintNodes, blueprintEdges, blueprintLegend } from './blueprint.js';

// Every node carries all 8 handles (both types, each side) so edges attach on any
// side; handle dots are hidden in CSS — this is a static, non-interactive diagram.
const HANDLES = [
  ['tt', 'target', Position.Top], ['ts', 'source', Position.Top],
  ['bt', 'target', Position.Bottom], ['bs', 'source', Position.Bottom],
  ['lt', 'target', Position.Left], ['ls', 'source', Position.Left],
  ['rt', 'target', Position.Right], ['rs', 'source', Position.Right],
];
const Handles = () =>
  HANDLES.map(([id, type, pos]) => (
    <Handle key={id} id={id} type={type} position={pos} isConnectable={false} />
  ));

function CardNode({ data }) {
  const cls = `node-card ${data.cat}${data.dashed ? ' dashed' : ''}${data.big ? ' big' : ''}`;
  return (
    <div className={cls}>
      <Handles />
      {data.kick ? <div className="kick">{data.kick}</div> : null}
      <div className="title">{data.title}</div>
      {data.sub ? <div className="sub">{data.sub}</div> : null}
      {data.desc ? <div className="desc">{data.desc}</div> : null}
    </div>
  );
}

function ZoneNode({ data }) {
  return (
    <div className={`node-zone ${data.cat}${data.dashed ? ' dashed' : ''}`}>
      <Handles />
      <div className="tab">{data.title}</div>
      {data.note ? <div className="note">{data.note}</div> : null}
    </div>
  );
}

// Custom edge: the ORM → DB "umbra-spine". Drops out of the source, hugs the far
// left margin, then enters the target — keeping the deepest data path out of the
// busy centre (the same routing the hand-drawn canvas used).
function SpineEdge({ sourceX, sourceY, targetX, targetY, style, markerEnd, data }) {
  const mX = -6;
  const drop = 46;
  const midY = (sourceY + drop + targetY) / 2;
  const d = `M ${sourceX},${sourceY} L ${sourceX},${sourceY + drop} L ${mX},${sourceY + drop} L ${mX},${targetY} L ${targetX},${targetY}`;
  return (
    <>
      <path d={d} fill="none" className="react-flow__edge-path" style={style} markerEnd={markerEnd} />
      {data?.label ? (
        <text className="spine-label" x={mX + 13} y={midY} transform={`rotate(-90 ${mX + 13} ${midY})`} textAnchor="middle">
          {data.label}
        </text>
      ) : null}
    </>
  );
}

const nodeTypes = { card: CardNode, zone: ZoneNode };
const edgeTypes = { spine: SpineEdge };

function downloadPng(nodes) {
  const bounds = getNodesBounds(nodes);
  const pad = 90;
  const width = Math.ceil(bounds.width + pad * 2);
  const height = Math.ceil(bounds.height + pad * 2);
  const vp = getViewportForBounds(bounds, width, height, 0.5, 2, pad);
  const el = document.querySelector('.react-flow__viewport');
  toPng(el, {
    backgroundColor: '#070912', width, height, pixelRatio: 2,
    style: { width: `${width}px`, height: `${height}px`, transform: `translate(${vp.x}px, ${vp.y}px) scale(${vp.zoom})` },
  }).then((url) => {
    const a = document.createElement('a');
    a.download = 'umbral-architecture-flow.png';
    a.href = url;
    a.click();
  });
}

function Flow({ view, setView }) {
  const isBp = view === 'blueprint';
  const [nodes, , onNodesChange] = useNodesState(isBp ? blueprintNodes : initialNodes);
  const [edges, , onEdgesChange] = useEdgesState(isBp ? blueprintEdges : initialEdges);
  const legend = isBp ? blueprintLegend : legendItems;
  const onExport = useCallback(() => downloadPng(nodes), [nodes]);

  return (
    <ReactFlow
      nodes={nodes}
      edges={edges}
      nodeTypes={nodeTypes}
      edgeTypes={edgeTypes}
      onNodesChange={onNodesChange}
      onEdgesChange={onEdgesChange}
      fitView
      fitViewOptions={{ padding: 0.06 }}
      minZoom={0.1}
      proOptions={{ hideAttribution: true }}
      nodesConnectable={false}
      edgesFocusable={false}
    >
      <Background variant={BackgroundVariant.Lines} gap={40} lineWidth={1} color="rgba(124,108,240,.06)" />
      <Background id="fine" variant={BackgroundVariant.Dots} gap={8} size={1} color="rgba(124,108,240,.05)" />
      <Controls showInteractive={false} />
      <MiniMap
        pannable zoomable
        nodeColor={(n) => (n.type === 'zone' ? 'rgba(124,108,240,.14)' : '#3b4166')}
        maskColor="rgba(7,9,18,.72)"
        style={{ background: '#0b0e1c' }}
      />
      <Panel position="top-left">
        <div className="panel-card toolbar">
          <img className="brand-mark" src="./umbral-mark.svg" width="30" height="30" alt="" />
          <span className="brand"><b>Umbral</b><span className="of">of the shadow · schematic</span></span>
          <span className="sep" />
          <div className="switch">
            <button className={view === 'flow' ? 'on' : ''} onClick={() => setView('flow')}>Flow</button>
            <button className={view === 'blueprint' ? 'on' : ''} onClick={() => setView('blueprint')}>Blueprint</button>
          </div>
          <span className="sep" />
          <button onClick={onExport}>Download PNG</button>
        </div>
      </Panel>
      <Panel position="top-right">
        <div className="panel-card legend">
          <div className="head">Map key</div>
          {legend.map(([label, color]) => (
            <div className="item" key={label}>
              <span className={`sw${label.includes('Roadmap') ? ' dash' : ''}`} style={{ background: color }} />
              {label}
            </div>
          ))}
        </div>
      </Panel>
    </ReactFlow>
  );
}

export default function App() {
  const [view, setView] = React.useState(
    () => (new URLSearchParams(window.location.search).get('view') === 'blueprint' ? 'blueprint' : 'flow'),
  );
  // keep the view shareable via ?view=blueprint
  const changeView = (v) => {
    setView(v);
    const u = new URL(window.location.href);
    if (v === 'flow') u.searchParams.delete('view');
    else u.searchParams.set('view', v);
    window.history.replaceState(null, '', u);
  };
  return (
    <div style={{ width: '100vw', height: '100vh' }}>
      <ReactFlowProvider>
        <Flow key={view} view={view} setView={changeView} />
      </ReactFlowProvider>
    </div>
  );
}
