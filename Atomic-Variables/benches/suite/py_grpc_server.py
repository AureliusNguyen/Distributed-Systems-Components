"""Baseline: the counter service a Python team would typically write. A grpcio
server implementing proto/atomvar.proto with a dict guarded by threading.Lock.

    python py_grpc_server.py <stub_dir> <port>
"""

import sys
import threading
from concurrent import futures

sys.path.insert(0, sys.argv[1])
import grpc  # noqa: E402
import atomvar_pb2 as pb  # noqa: E402
import atomvar_pb2_grpc as pbg  # noqa: E402


class Counter(pbg.AtomicServiceServicer):
    def __init__(self):
        self.lock = threading.Lock()
        self.vals = {}

    @staticmethod
    def key(var):
        return (var.arena, var.name)

    def Create(self, req, ctx):
        with self.lock:
            v = self.vals.setdefault(self.key(req.var), req.init.i64)
        return pb.ValueResponse(type=pb.VALUE_TYPE_I64, value=pb.Value(i64=v))

    def Get(self, req, ctx):
        with self.lock:
            v = self.vals[self.key(req.var)]
        return pb.ValueResponse(type=pb.VALUE_TYPE_I64, value=pb.Value(i64=v))

    def Fetch(self, req, ctx):
        k = self.key(req.var)
        with self.lock:
            prev = self.vals[k]
            self.vals[k] = prev + req.operand.i64
        return pb.FetchResponse(previous=pb.Value(i64=prev), current=pb.Value(i64=prev + req.operand.i64))

    def CompareExchange(self, req, ctx):
        k = self.key(req.var)
        with self.lock:
            prev = self.vals[k]
            ok = prev == req.expected.i64
            if ok:
                self.vals[k] = req.desired.i64
        return pb.CompareExchangeResponse(exchanged=ok, previous=pb.Value(i64=prev))


def main():
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=16))
    pbg.add_AtomicServiceServicer_to_server(Counter(), server)
    server.add_insecure_port(f"127.0.0.1:{sys.argv[2]}")
    server.start()
    print("ready", flush=True)
    server.wait_for_termination()


if __name__ == "__main__":
    main()
