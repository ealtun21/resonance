import socket,sys,json,time
x,y=int(sys.argv[1]),int(sys.argv[2]); n=int(sys.argv[3]) if len(sys.argv)>3 else 1
s=socket.socket(socket.AF_UNIX); s.connect('/tmp/mac-qmp.sock'); f=s.makefile('rw')
f.readline()
def c(o):
    f.write(json.dumps(o)+'\n'); f.flush(); return f.readline()
c({"execute":"qmp_capabilities"})
def ev(evs): return c({"execute":"input-send-event","arguments":{"events":evs}})
ax=lambda a,v:{"type":"abs","data":{"axis":a,"value":v}}
ev([ax("x",int(x/1920*32767)),ax("y",int(y/1080*32767))]); time.sleep(0.3)
for _ in range(n):
    ev([{"type":"btn","data":{"down":True,"button":"left"}}]); time.sleep(0.08)
    ev([{"type":"btn","data":{"down":False,"button":"left"}}]); time.sleep(0.2)
