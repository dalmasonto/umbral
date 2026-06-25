import{F as S,be as R,g as Q,a1 as Y,b1 as tt,a2 as et,b2 as at,a6 as rt,b4 as nt,b as p,aE as F,a4 as it,s as st,b0 as lt,aN as ot,E as ct,t as ut,S as pt}from"./DQX82s-F.js";import{p as gt}from"./DY0DCM0G.js";import{p as dt}from"./DgHNyr-s.js";import{d as _}from"./ypSj6b-E.js";import{o as ft}from"./CmKTTxBW.js";function ht(t,a){return a<t?-1:a>t?1:a>=t?0:NaN}function mt(t){return t}function vt(){var t=mt,a=ht,f=null,y=S(0),s=S(R),g=S(0);function l(e){var n,o=(e=Q(e)).length,d,h,v=0,c=new Array(o),i=new Array(o),x=+y.apply(this,arguments),w=Math.min(R,Math.max(-R,s.apply(this,arguments)-x)),m,D=Math.min(Math.abs(w)/o,g.apply(this,arguments)),$=D*(w<0?-1:1),u;for(n=0;n<o;++n)(u=i[c[n]=n]=+t(e[n],n,e))>0&&(v+=u);for(a!=null?c.sort(function(A,C){return a(i[A],i[C])}):f!=null&&c.sort(function(A,C){return f(e[A],e[C])}),n=0,h=v?(w-o*$)/v:0;n<o;++n,x=m)d=c[n],u=i[d],m=x+(u>0?u*h:0)+$,i[d]={data:e[d],index:n,value:u,startAngle:x,endAngle:m,padAngle:D};return i}return l.value=function(e){return arguments.length?(t=typeof e=="function"?e:S(+e),l):t},l.sortValues=function(e){return arguments.length?(a=e,f=null,l):a},l.sort=function(e){return arguments.length?(f=e,a=null,l):f},l.startAngle=function(e){return arguments.length?(y=typeof e=="function"?e:S(+e),l):y},l.endAngle=function(e){return arguments.length?(s=typeof e=="function"?e:S(+e),l):s},l.padAngle=function(e){return arguments.length?(g=typeof e=="function"?e:S(+e),l):g},l}var xt=pt.pie,W={sections:new Map,showData:!1},T=W.sections,z=W.showData,St=structuredClone(xt),yt=p(()=>structuredClone(St),"getConfig"),wt=p(()=>{T=new Map,z=W.showData,ut()},"clear"),At=p(({label:t,value:a})=>{if(a<0)throw new Error(`"${t}" has invalid value: ${a}. Negative values are not allowed in pie charts. All slice values must be >= 0.`);T.has(t)||(T.set(t,a),F.debug(`added new section: ${t}, with value: ${a}`))},"addSection"),Ct=p(()=>T,"getSections"),Dt=p(t=>{z=t},"setShowData"),$t=p(()=>z,"getShowData"),V={getConfig:yt,clear:wt,setDiagramTitle:nt,getDiagramTitle:rt,setAccTitle:at,getAccTitle:et,setAccDescription:tt,getAccDescription:Y,addSection:At,getSections:Ct,setShowData:Dt,getShowData:$t},Tt=p((t,a)=>{gt(t,a),a.setShowData(t.showData),t.sections.map(a.addSection)},"populateDb"),bt={parse:p(async t=>{const a=await dt("pie",t);F.debug(a),Tt(a,V)},"parse")},Et=p(t=>`
  .pieCircle{
    stroke: ${t.pieStrokeColor};
    stroke-width : ${t.pieStrokeWidth};
    opacity : ${t.pieOpacity};
  }
  .pieOuterCircle{
    stroke: ${t.pieOuterStrokeColor};
    stroke-width: ${t.pieOuterStrokeWidth};
    fill: none;
  }
  .pieTitleText {
    text-anchor: middle;
    font-size: ${t.pieTitleTextSize};
    fill: ${t.pieTitleTextColor};
    font-family: ${t.fontFamily};
  }
  .slice {
    font-family: ${t.fontFamily};
    fill: ${t.pieSectionTextColor};
    font-size:${t.pieSectionTextSize};
    // fill: white;
  }
  .legend text {
    fill: ${t.pieLegendTextColor};
    font-family: ${t.fontFamily};
    font-size: ${t.pieLegendTextSize};
  }
`,"getStyles"),kt=Et,Mt=p(t=>{const a=[...t.values()].reduce((s,g)=>s+g,0),f=[...t.entries()].map(([s,g])=>({label:s,value:g})).filter(s=>s.value/a*100>=1);return vt().value(s=>s.value).sort(null)(f)},"createPieArcs"),Rt=p((t,a,f,y)=>{var P;F.debug(`rendering pie chart
`+t);const s=y.db,g=it(),l=st(s.getConfig(),g.pie),e=40,n=18,o=4,d=450,h=d,v=lt(a),c=v.append("g");c.attr("transform","translate("+h/2+","+d/2+")");const{themeVariables:i}=g;let[x]=ot(i.pieOuterStrokeWidth);x??(x=2);const w=l.textPosition,m=Math.min(h,d)/2-e,D=_().innerRadius(0).outerRadius(m),$=_().innerRadius(m*w).outerRadius(m*w);c.append("circle").attr("cx",0).attr("cy",0).attr("r",m+x/2).attr("class","pieOuterCircle");const u=s.getSections(),A=Mt(u),C=[i.pie1,i.pie2,i.pie3,i.pie4,i.pie5,i.pie6,i.pie7,i.pie8,i.pie9,i.pie10,i.pie11,i.pie12];let b=0;u.forEach(r=>{b+=r});const N=A.filter(r=>(r.data.value/b*100).toFixed(0)!=="0"),E=ft(C).domain([...u.keys()]);c.selectAll("mySlices").data(N).enter().append("path").attr("d",D).attr("fill",r=>E(r.data.label)).attr("class","pieCircle"),c.selectAll("mySlices").data(N).enter().append("text").text(r=>(r.data.value/b*100).toFixed(0)+"%").attr("transform",r=>"translate("+$.centroid(r)+")").style("text-anchor","middle").attr("class","slice");const U=c.append("text").text(s.getDiagramTitle()).attr("x",0).attr("y",-400/2).attr("class","pieTitleText"),G=[...u.entries()].map(([r,M])=>({label:r,value:M})),k=c.selectAll(".legend").data(G).enter().append("g").attr("class","legend").attr("transform",(r,M)=>{const I=n+o,H=I*G.length/2,J=12*n,K=M*I-H;return"translate("+J+","+K+")"});k.append("rect").attr("width",n).attr("height",n).style("fill",r=>E(r.label)).style("stroke",r=>E(r.label)),k.append("text").attr("x",n+o).attr("y",n-o).text(r=>s.getShowData()?`${r.label} [${r.value}]`:r.label);const j=Math.max(...k.selectAll("text").nodes().map(r=>(r==null?void 0:r.getBoundingClientRect().width)??0)),X=h+e+n+o+j,L=((P=U.node())==null?void 0:P.getBoundingClientRect().width)??0,Z=h/2-L/2,q=h/2+L/2,B=Math.min(0,Z),O=Math.max(X,q)-B;v.attr("viewBox",`${B} 0 ${O} ${d}`),ct(v,d,O,l.useMaxWidth)},"draw"),Ft={draw:Rt},Ot={parser:bt,db:V,renderer:Ft,styles:kt};export{Ot as diagram};
