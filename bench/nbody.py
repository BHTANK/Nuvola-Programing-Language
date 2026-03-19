from math import sqrt
sun_x=sun_y=sun_z=sun_vx=sun_vy=sun_vz=0.0; sun_m=1.0
jup_x=4.84143144246472090; jup_y=-1.16032004402742839; jup_z=-0.10327910045777416
jup_vx=0.00166003245*365.25; jup_vy=0.007699011275*365.25; jup_vz=-0.0000690460016*365.25; jup_m=0.000954791938
sat_x=8.34336671824457987; sat_y=4.12479856412430479; sat_z=-0.40352044297
sat_vx=-0.002767425107*365.25; sat_vy=0.004998528012*365.25; sat_vz=0.0000230417297*365.25; sat_m=0.000285885980
dt=0.01
for _ in range(500000):
    dx,dy,dz=sun_x-jup_x,sun_y-jup_y,sun_z-jup_z
    d2=dx*dx+dy*dy+dz*dz; d=sqrt(d2); mag=dt/(d2*d)
    sun_vx-=dx*jup_m*mag; sun_vy-=dy*jup_m*mag; sun_vz-=dz*jup_m*mag
    jup_vx+=dx*sun_m*mag; jup_vy+=dy*sun_m*mag; jup_vz+=dz*sun_m*mag
    dx,dy,dz=sun_x-sat_x,sun_y-sat_y,sun_z-sat_z
    d2=dx*dx+dy*dy+dz*dz; d=sqrt(d2); mag=dt/(d2*d)
    sun_vx-=dx*sat_m*mag; sun_vy-=dy*sat_m*mag; sun_vz-=dz*sat_m*mag
    sat_vx+=dx*sun_m*mag; sat_vy+=dy*sun_m*mag; sat_vz+=dz*sun_m*mag
    dx,dy,dz=jup_x-sat_x,jup_y-sat_y,jup_z-sat_z
    d2=dx*dx+dy*dy+dz*dz; d=sqrt(d2); mag=dt/(d2*d)
    jup_vx-=dx*sat_m*mag; jup_vy-=dy*sat_m*mag; jup_vz-=dz*sat_m*mag
    sat_vx+=dx*jup_m*mag; sat_vy+=dy*jup_m*mag; sat_vz+=dz*jup_m*mag
    sun_x+=dt*sun_vx; sun_y+=dt*sun_vy; sun_z+=dt*sun_vz
    jup_x+=dt*jup_vx; jup_y+=dt*jup_vy; jup_z+=dt*jup_vz
    sat_x+=dt*sat_vx; sat_y+=dt*sat_vy; sat_z+=dt*sat_vz
ke = sun_m*(sun_vx**2+sun_vy**2+sun_vz**2)*0.5 + jup_m*(jup_vx**2+jup_vy**2+jup_vz**2)*0.5 + sat_m*(sat_vx**2+sat_vy**2+sat_vz**2)*0.5
print(f"nbody 500k steps, KE = {ke:g}")
