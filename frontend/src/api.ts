export type DSLFile={path:string,name:string}
export type Snapshot={cycle_index:number,simulation_time:number,values:Record<string,unknown>}
export type TrendDescriptor={name:string,scale:number}
export type TagDescriptor={name:string,data_type:string,writable:boolean,unit:string,description:string,initial:unknown}
export type Features={metadata:Record<string,unknown>,tags:TagDescriptor[],defaultTrends:TrendDescriptor[],defaultExportTags:string[]}
export type Job={id:string,artifact:string,running:boolean}
export type RunResult={job:Job,features:Features}
export type SimulationResult={features:Features,snapshots:Snapshot[]}

export type ExportOptions={fileType:'csv';titleRows:number;features:string[];startTime?:number;endTime?:number}
type Backend={SelectWorkspace():Promise<string>;SetWorkspace(path:string):Promise<void>;ListDSL():Promise<DSLFile[]>;Inspect(path:string):Promise<Features>;Simulate(path:string,cycles:number,sampleCount:number):Promise<SimulationResult>;GenerateData(path:string,cycles:number,sampleInterval:number,options:ExportOptions):Promise<string>;StartDebug(path:string):Promise<RunResult>;Execute(id:string,cycles:number,writes:Record<string,unknown>):Promise<Snapshot[]>;Trend(id:string,offset:number):Promise<Snapshot[]>;StopDebug(id:string):Promise<void>;Export(id:string,options:ExportOptions):Promise<string>}
declare global{interface Window{go?:{application?:{App?:Backend}}}}
function backend():Backend{const app=window.go?.application?.App;if(!app)throw new Error('Studio 后端尚未连接');return app}
export const api={selectWorkspace:()=>backend().SelectWorkspace(),setWorkspace:(p:string)=>backend().SetWorkspace(p),listDSL:()=>backend().ListDSL(),inspect:(p:string)=>backend().Inspect(p),simulate:(p:string,n:number,c:number)=>backend().Simulate(p,n,c),generateData:(p:string,n:number,i:number,o:ExportOptions)=>backend().GenerateData(p,n,i,o),start:(p:string)=>backend().StartDebug(p),execute:(id:string,n:number,w:Record<string,unknown>)=>backend().Execute(id,n,w),trend:(id:string,o:number)=>backend().Trend(id,o),stop:(id:string)=>backend().StopDebug(id),exportData:(id:string,o:ExportOptions)=>backend().Export(id,o),exportCSV:(id:string)=>backend().Export(id,{fileType:'csv',titleRows:1,features:[]})}
