export namespace application {
	
	export class ExportOptions {
	    fileType: string;
	    titleRows: number;
	    features: string[];
	    startTime?: number;
	    endTime?: number;
	
	    static createFrom(source: any = {}) {
	        return new ExportOptions(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.fileType = source["fileType"];
	        this.titleRows = source["titleRows"];
	        this.features = source["features"];
	        this.startTime = source["startTime"];
	        this.endTime = source["endTime"];
	    }
	}
	export class Features {
	    metadata: any;
	    tags: any;
	    defaultTrends: any;
	    defaultExportTags: any;
	
	    static createFrom(source: any = {}) {
	        return new Features(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.metadata = source["metadata"];
	        this.tags = source["tags"];
	        this.defaultTrends = source["defaultTrends"];
	        this.defaultExportTags = source["defaultExportTags"];
	    }
	}
	export class RunResult {
	    job: debug.Job;
	    features: Features;
	
	    static createFrom(source: any = {}) {
	        return new RunResult(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.job = this.convertValues(source["job"], debug.Job);
	        this.features = this.convertValues(source["features"], Features);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	export class SimulationResult {
	    features: Features;
	    snapshots: debug.Snapshot[];
	
	    static createFrom(source: any = {}) {
	        return new SimulationResult(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.features = this.convertValues(source["features"], Features);
	        this.snapshots = this.convertValues(source["snapshots"], debug.Snapshot);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}

}

export namespace debug {
	
	export class Job {
	    id: string;
	    artifact: string;
	    running: boolean;
	
	    static createFrom(source: any = {}) {
	        return new Job(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.id = source["id"];
	        this.artifact = source["artifact"];
	        this.running = source["running"];
	    }
	}
	export class Snapshot {
	    cycle_index: number;
	    simulation_time: number;
	    values: Record<string, any>;
	
	    static createFrom(source: any = {}) {
	        return new Snapshot(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.cycle_index = source["cycle_index"];
	        this.simulation_time = source["simulation_time"];
	        this.values = source["values"];
	    }
	}

}

export namespace files {
	
	export class File {
	    path: string;
	    name: string;
	
	    static createFrom(source: any = {}) {
	        return new File(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.path = source["path"];
	        this.name = source["name"];
	    }
	}

}

